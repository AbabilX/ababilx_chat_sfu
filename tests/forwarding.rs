mod common;

use std::time::{Duration, Instant};

use common::browser::Browser;

/// Drives every browser's loop until `done` holds, or fails the test.
fn pump_until(browsers: &mut [Browser], label: &str, timeout: Duration, done: impl Fn(&[Browser]) -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        for browser in browsers.iter_mut() {
            browser.step();
        }
        if done(browsers) {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("timed out waiting for {label}");
}

#[test]
fn two_peers_join_and_audio_is_forwarded() {
    let server = common::start_server();

    let mut browsers = vec![
        Browser::join("alice", &server.ws_url(), &server.token("room-1", "user-alice")),
        Browser::join("bob", &server.ws_url(), &server.token("room-1", "user-bob")),
    ];

    // The SFU answers the join offer over the signaling socket.
    pump_until(&mut browsers, "both joins to be answered", Duration::from_secs(10), |all| {
        all.iter().all(|browser| browser.joined)
    });

    // ICE and DTLS complete over the shared UDP port.
    pump_until(&mut browsers, "ICE to connect", Duration::from_secs(15), |all| {
        all.iter().all(|browser| browser.connected)
    });

    // With both publishing, each must receive the other's packets. Receiving
    // anything at all means the SFU negotiated a subscription m-line, matched
    // the payload type and rewrote the stream onto it.
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        for browser in browsers.iter_mut() {
            browser.send_audio();
            browser.step();
        }
        if browsers.iter().all(|browser| browser.received_packets >= 5) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    for browser in &browsers {
        assert!(
            browser.received_packets >= 5,
            "{} received {} packets from the other peer",
            browser.label,
            browser.received_packets,
        );
    }
}

#[test]
fn a_bad_token_is_refused_before_any_media() {
    let server = common::start_server();
    let (mut socket, _) = tungstenite::connect(server.ws_url()).expect("connect");
    socket
        .send(tungstenite::Message::Text(
            serde_json::json!({"t": "join", "token": "not-a-token", "sdp": "v=0\r\n"})
                .to_string(),
        ))
        .expect("send join");
    let message = socket.read().expect("a reply");
    let text = message.into_text().expect("text frame");
    let value: serde_json::Value = serde_json::from_str(&text).expect("json");
    assert_eq!(value["t"], "error");
    assert_eq!(value["code"], "unauthorized");
}

#[test]
fn a_third_peer_receives_every_other_publisher() {
    let server = common::start_server();
    let mut browsers: Vec<Browser> = ["carol", "dave", "erin"]
        .iter()
        .map(|name| {
            Browser::join(
                name,
                &server.ws_url(),
                &server.token("room-3", &format!("user-{name}")),
            )
        })
        .collect();

    pump_until(&mut browsers, "three joins", Duration::from_secs(10), |all| {
        all.iter().all(|browser| browser.joined)
    });
    pump_until(&mut browsers, "three ICE connections", Duration::from_secs(20), |all| {
        all.iter().all(|browser| browser.connected)
    });

    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        for browser in browsers.iter_mut() {
            browser.send_audio();
            browser.step();
        }
        if browsers.iter().all(|browser| browser.received_mids.len() >= 2) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    for browser in &browsers {
        assert_eq!(
            browser.received_mids.len(),
            2,
            "{} should receive both other publishers, got {}",
            browser.label,
            browser.received_mids.len(),
        );
    }

    // Everyone learned about the two who arrived after them, or before them via
    // the roster in the join reply.
    assert!(browsers[0].events("peer_joined") >= 2, "carol saw the later arrivals");
}

#[test]
fn a_departure_reaches_the_rest_of_the_room() {
    let server = common::start_server();
    let mut browsers = vec![
        Browser::join("frank", &server.ws_url(), &server.token("room-4", "user-frank")),
        Browser::join("grace", &server.ws_url(), &server.token("room-4", "user-grace")),
    ];
    pump_until(&mut browsers, "both joins", Duration::from_secs(10), |all| {
        all.iter().all(|browser| browser.joined)
    });

    browsers[1].hang_up();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && browsers[0].events("peer_left") == 0 {
        browsers[0].step();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(browsers[0].events("peer_left"), 1, "frank should be told grace left");
}
