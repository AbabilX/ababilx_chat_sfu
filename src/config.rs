use std::env;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

/// Everything the SFU reads from the environment, resolved once at boot.
///
/// `public_ip` is the address that ends up in the ICE host candidate we hand
/// browsers. On a VPS that is the box's own public IPv4, so it must be stated
/// explicitly — guessing from the default route works for a laptop and is wrong
/// behind any kind of NAT.
#[derive(Debug, Clone)]
pub struct Config {
    pub http_bind: SocketAddr,
    pub udp_bind: SocketAddr,
    pub public_ip: IpAddr,
    pub shared_secret: String,
    /// Where room lifecycle events are POSTed. The only coupling to a host
    /// application, and it is optional: with no URL the SFU simply runs and
    /// reports nothing.
    pub webhook_url: Option<String>,
    pub webhook_secret: String,
    pub ice_lite: bool,
    pub max_room_peers: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0} is required")]
    Missing(&'static str),
    #[error("{0} is not a valid {1}")]
    Invalid(String, &'static str),
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let http_bind = parse_addr("SFU_BIND_HTTP", "0.0.0.0:7898")?;
        let udp_bind = parse_addr("SFU_BIND_UDP", "0.0.0.0:7899")?;
        let shared_secret = required("SFU_SHARED_SECRET")?;
        let webhook_url = env::var("SFU_WEBHOOK_URL")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let webhook_secret = env::var("SFU_WEBHOOK_SECRET")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| shared_secret.clone());
        let public_ip = match env::var("SFU_PUBLIC_IP") {
            Ok(value) if !value.trim().is_empty() => value
                .trim()
                .parse()
                .map_err(|_| ConfigError::Invalid(value, "IP address"))?,
            _ => guess_local_ip(),
        };
        Ok(Config {
            http_bind,
            udp_bind,
            public_ip,
            shared_secret,
            webhook_url,
            webhook_secret,
            ice_lite: flag("SFU_ICE_LITE", false),
            max_room_peers: number("SFU_MAX_ROOM_PEERS", 16, 2, 64),
        })
    }

    /// The address a browser is told to send media to.
    pub fn advertised_udp(&self, bound: SocketAddr) -> SocketAddr {
        SocketAddr::new(self.public_ip, bound.port())
    }
}

fn required(key: &'static str) -> Result<String, ConfigError> {
    match env::var(key) {
        Ok(value) if !value.trim().is_empty() => Ok(value.trim().to_string()),
        _ => Err(ConfigError::Missing(key)),
    }
}

fn parse_addr(key: &'static str, fallback: &str) -> Result<SocketAddr, ConfigError> {
    let raw = env::var(key).unwrap_or_else(|_| fallback.to_string());
    raw.trim()
        .parse()
        .map_err(|_| ConfigError::Invalid(raw, "socket address"))
}

fn flag(key: &str, fallback: bool) -> bool {
    match env::var(key) {
        Ok(value) => matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"),
        Err(_) => fallback,
    }
}

fn number(key: &str, fallback: usize, min: usize, max: usize) -> usize {
    env::var(key)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value >= min && *value <= max)
        .unwrap_or(fallback)
}

/// Opens a throwaway UDP socket towards a public address so the OS picks the
/// outbound interface for us. Nothing is sent; connect() on UDP only sets the
/// default peer. Falls back to loopback, which is correct for local dev.
fn guess_local_ip() -> IpAddr {
    UdpSocket::bind("0.0.0.0:0")
        .and_then(|socket| {
            socket.connect("1.1.1.1:80")?;
            socket.local_addr()
        })
        .map(|addr| addr.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
}
