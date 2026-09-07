use std::collections::HashSet;
use std::io::ErrorKind;
use std::net::{TcpStream, UdpSocket};
use std::time::Instant;

use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::media::{Direction, Frequency, MediaKind, MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

/// One Opus frame at 20 ms, 48 kHz.
const SAMPLES_PER_FRAME: u64 = 960;

/// Stands in for a browser tab: it offers one send-only audio m-line, answers
/// whatever the SFU offers back, and counts the media it receives.
pub struct Browser {
    pub label: &'static str,
    pub joined: bool,
    pub connected: bool,
    pub received_packets: usize,
    /// One entry per distinct inbound m-line, i.e. per remote publisher.
    pub received_mids: HashSet<Mid>,
    pub peer_events: Vec<String>,
    rtc: Rtc,
    socket: UdpSocket,
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
    pending: Option<SdpPendingOffer>,
    audio_mid: Mid,
    rtp_time: u64,
    buf: Vec<u8>,
}

impl Browser {
    pub fn join(label: &'static str, ws_url: &str, token: &str) -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").expect("bind udp");
        socket.set_nonblocking(true).expect("nonblocking udp");
        let mut rtc = Rtc::builder().build(Instant::now());
        let candidate =
            Candidate::host(socket.local_addr().expect("local addr"), "udp").expect("candidate");
        rtc.add_local_candidate(candidate);

        let mut change = rtc.sdp_api();
        // Exactly what the web client offers: the microphone, send-only.
        let audio_mid = change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
        let (offer, pending) = change.apply().expect("initial offer");

        let (mut ws, _) = tungstenite::connect(ws_url).expect("connect ws");
        ws.send(Message::Text(
            serde_json::json!({"t": "join", "token": token, "sdp": offer.to_sdp_string()})
                .to_string(),
        ))
        .expect("send join");
        if let MaybeTlsStream::Plain(stream) = ws.get_mut() {
            stream.set_nonblocking(true).expect("nonblocking ws");
        }

        Browser {
            label,
            joined: false,
            connected: false,
            received_packets: 0,
            received_mids: HashSet::new(),
            peer_events: Vec::new(),
            rtc,
            socket,
            ws,
            pending: Some(pending),
            audio_mid,
            rtp_time: 0,
            buf: vec![0; 2000],
        }
    }

    /// One turn of this browser's event loop.
    pub fn step(&mut self) {
        self.drain_signaling();
        loop {
            match self.rtc.poll_output() {
                Ok(Output::Transmit(transmit)) => {
                    let _ = self.socket.send_to(&transmit.contents, transmit.destination);
                }
                Ok(Output::Timeout(_)) => break,
                Ok(Output::Event(event)) => self.on_event(event),
                Err(_) => return,
            }
        }
        self.read_udp();
        let _ = self.rtc.handle_input(Input::Timeout(Instant::now()));
    }

    /// Publishes one 20 ms frame of (meaningless) Opus payload.
    pub fn send_audio(&mut self) {
        if !self.connected {
            return;
        }
        let Some(writer) = self.rtc.writer(self.audio_mid) else { return };
        let Some(pt) = writer.payload_params().map(|params| params.pt()).next() else {
            return;
        };
        let time = MediaTime::new(self.rtp_time, Frequency::FORTY_EIGHT_KHZ);
        let _ = writer.write(pt, Instant::now(), time, vec![0x0a_u8; 80]);
        self.rtp_time += SAMPLES_PER_FRAME;
    }

    pub fn events(&self, kind: &str) -> usize {
        self.peer_events.iter().filter(|event| *event == kind).count()
    }

    /// Closes the signaling socket the way a shut tab would.
    pub fn hang_up(&mut self) {
        let _ = self.ws.close(None);
        let _ = self.ws.flush();
    }

    fn drain_signaling(&mut self) {
        loop {
            match self.ws.read() {
                Ok(Message::Text(text)) => self.on_signal(&text),
                Ok(_) => {}
                Err(tungstenite::Error::Io(error)) if error.kind() == ErrorKind::WouldBlock => {
                    return
                }
                Err(_) => return,
            }
        }
    }

    fn on_signal(&mut self, text: &str) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else { return };
        match value["t"].as_str().unwrap_or_default() {
            "joined" => {
                let sdp = value["sdp"].as_str().unwrap_or_default();
                let answer = SdpAnswer::from_sdp_string(sdp).expect("answer parses");
                let pending = self.pending.take().expect("pending offer");
                self.rtc
                    .sdp_api()
                    .accept_answer(pending, answer)
                    .expect("answer accepted");
                self.joined = true;
            }
            "offer" => {
                let sdp = value["sdp"].as_str().unwrap_or_default();
                let offer = SdpOffer::from_sdp_string(sdp).expect("offer parses");
                let answer = self
                    .rtc
                    .sdp_api()
                    .accept_offer(offer)
                    .expect("offer accepted");
                let _ = self.ws.send(Message::Text(
                    serde_json::json!({"t": "answer", "sdp": answer.to_sdp_string()})
                        .to_string(),
                ));
            }
            "error" => panic!(
                "{} received error {}",
                self.label,
                value["code"].as_str().unwrap_or_default()
            ),
            other => self.peer_events.push(other.to_string()),
        }
    }

    fn on_event(&mut self, event: Event) {
        match event {
            Event::IceConnectionStateChange(state) => {
                if matches!(state, IceConnectionState::Connected | IceConnectionState::Completed) {
                    self.connected = true;
                }
            }
            Event::MediaData(data) => {
                self.received_packets += 1;
                self.received_mids.insert(data.mid);
            }
            _ => {}
        }
    }

    fn read_udp(&mut self) {
        loop {
            self.buf.resize(2000, 0);
            let (len, source) = match self.socket.recv_from(&mut self.buf) {
                Ok(value) => value,
                Err(_) => return,
            };
            self.buf.truncate(len);
            let Ok(contents) = self.buf.as_slice().try_into() else { continue };
            let destination = self.socket.local_addr().expect("local addr");
            let input = Input::Receive(
                Instant::now(),
                Receive { proto: Protocol::Udp, source, destination, contents },
            );
            if self.rtc.accepts(&input) {
                let _ = self.rtc.handle_input(input);
            }
        }
    }
}
