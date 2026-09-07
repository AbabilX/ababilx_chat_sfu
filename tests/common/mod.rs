//! A headless test rig: a real SFU process in-thread, and "browsers" built out
//! of str0m so the whole path — WebSocket signaling, SDP, ICE, DTLS, SRTP,
//! forwarding — is exercised without a browser anywhere.

pub mod browser;

use std::net::{IpAddr, Ipv4Addr, TcpListener};
use std::sync::Arc;

use ferrite_sfu::{config::Config, sfu, signal};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde_json::json;

pub struct TestServer {
    /// The signaling endpoint as a URL, not a socket address: a deployment is
    /// reached by hostname over TLS through a proxy, and its media port is the
    /// only one published directly.
    pub ws_url: String,
    pub secret: String,
}

impl TestServer {
    pub fn ws_url(&self) -> String {
        self.ws_url.clone()
    }

    pub fn token(&self, room: &str, user: &str) -> String {
        let exp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 300;
        encode(
            &Header::new(Algorithm::HS256),
            &json!({
                "sub": user, "room": room, "call_id": "call-under-test",
                "name": user, "avatar_url": "", "exp": exp,
            }),
            &EncodingKey::from_secret(self.secret.as_bytes()),
        )
        .expect("mint token")
    }
}

/// Points the suite at an SFU that is already running — a container, a staging
/// box — instead of starting one in-process:
///
///   FERRITE_SFU_TEST_URL=ws://127.0.0.1:7898 \
///   FERRITE_SFU_TEST_SECRET=local-dev-secret cargo test
///
/// The same conformance tests then verify a real deployment, which is the only
/// way to check that the advertised ICE candidate and the published UDP port
/// actually work.
fn external_server() -> Option<TestServer> {
    let url = std::env::var("FERRITE_SFU_TEST_URL").ok()?;
    let secret = std::env::var("FERRITE_SFU_TEST_SECRET").ok()?;
    Some(TestServer {
        ws_url: normalize_ws_url(&url),
        secret,
    })
}

/// Accepts what an operator actually has to hand — `wss://sfu.example.com`,
/// `https://sfu.example.com/ws`, `127.0.0.1:7898` — and yields a URL tungstenite
/// can dial. `http`/`https` are translated rather than rejected because that is
/// what the browser-facing config holds.
fn normalize_ws_url(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    let with_scheme = if let Some(rest) = trimmed.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if trimmed.starts_with("ws://") || trimmed.starts_with("wss://") {
        trimmed.to_string()
    } else {
        format!("ws://{trimmed}")
    };
    if with_scheme.ends_with("/ws") {
        with_scheme
    } else {
        format!("{with_scheme}/ws")
    }
}

/// Boots the SFU on ephemeral ports. Everything binds to loopback, so the ICE
/// host candidate the SFU advertises is one the test clients can actually reach.
pub fn start_server() -> TestServer {
    if let Some(server) = external_server() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| str0m::crypto::from_feature_flags().install_process_default());
        return server;
    }
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| str0m::crypto::from_feature_flags().install_process_default());

    let secret = "integration-test-secret".to_string();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind http");
    let http = listener.local_addr().expect("http addr");
    listener.set_nonblocking(true).expect("nonblocking");

    let config = Arc::new(Config {
        http_bind: http,
        // Deliberately a wildcard bind with a specific advertised IP: that is
        // what a real deployment does, and binding the same loopback address it
        // advertises would hide an ICE candidate mismatch.
        udp_bind: "0.0.0.0:0".parse().expect("udp bind"),
        public_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
        shared_secret: secret.clone(),
        webhook_url: None,
        webhook_secret: secret.clone(),
        ice_lite: false,
        max_room_peers: 8,
    });

    let (report_tx, report_rx) = tokio::sync::mpsc::unbounded_channel();
    let (engine, _media) = sfu::spawn(config.clone(), report_tx).expect("spawn engine");

    std::thread::Builder::new()
        .name("sfu-test-http".to_string())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            runtime.block_on(async move {
                tokio::spawn(async move {
                    let mut rx = report_rx;
                    while rx.recv().await.is_some() {}
                });
                let listener = tokio::net::TcpListener::from_std(listener).expect("adopt listener");
                let state = signal::AppState { engine, config };
                let _ = axum::serve(listener, signal::router(state)).await;
            });
        })
        .expect("spawn http thread");

    TestServer {
        ws_url: format!("ws://{http}/ws"),
        secret,
    }
}
