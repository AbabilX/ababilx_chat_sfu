use std::net::UdpSocket;
use std::time::{Duration, Instant};

use str0m::change::SdpPendingOffer;
use str0m::{Event, IceConnectionState, Input, Output, Rtc};
use tokio::sync::mpsc::UnboundedSender;

use crate::proto::{PeerInfo, ServerMsg};

use super::tracks::{PeerId, Propagated, TrackInEntry, TrackOut};

/// How long a peer stays "speaking" after its last packet above the threshold.
/// Short enough to feel live, long enough that DTX gaps do not flicker the ring.
pub(super) const SPEAKING_HOLD: Duration = Duration::from_millis(600);

/// How long a peer may sit in ICE `Disconnected` before it is given up on.
///
/// `Disconnected` is transient by design — a WiFi blip, a route change, a
/// laptop lid. Killing the peer the instant it appears turns every hiccup into
/// a dropped call, which is what str0m's own example does and openly calls out
/// as a shortcut. Real clients recover well within this window.
pub(super) const ICE_DISCONNECT_GRACE: Duration = Duration::from_secs(20);

/// RFC 6464 reports -dBov, so smaller is louder. -50 is roughly "someone is
/// talking" and is what browsers use for their own speaking indicator.
pub(super) const SPEAKING_THRESHOLD_DBOV: i8 = 50;

pub struct Peer {
    pub id: PeerId,
    pub user_id: String,
    pub name: String,
    pub avatar_url: String,
    pub rtc: Rtc,
    pub pending: Option<SdpPendingOffer>,
    pub out: UnboundedSender<ServerMsg>,
    pub tracks_in: Vec<TrackInEntry>,
    pub tracks_out: Vec<TrackOut>,
    pub muted: bool,
    pub screen: bool,
    /// True once ICE is up. Media cannot flow before this, so it is the honest
    /// moment to tell the host application somebody actually arrived.
    pub connected: bool,
    /// When ICE last went `Disconnected`, cleared the moment it recovers.
    pub disconnected_since: Option<Instant>,
    /// Set once that arrival has been reported, so a reconnect storm cannot
    /// double-report and inflate a participant peak.
    pub reported: bool,
    pub speaking_until: Option<Instant>,
}

impl Peer {
    pub fn new(
        id: PeerId,
        rtc: Rtc,
        user_id: String,
        name: String,
        avatar_url: String,
        out: UnboundedSender<ServerMsg>,
    ) -> Self {
        Peer {
            id,
            user_id,
            name,
            avatar_url,
            rtc,
            pending: None,
            out,
            tracks_in: Vec::new(),
            tracks_out: Vec::new(),
            muted: false,
            screen: false,
            connected: false,
            disconnected_since: None,
            reported: false,
            speaking_until: None,
        }
    }

    pub fn info(&self) -> PeerInfo {
        PeerInfo {
            peer_id: self.id.as_string(),
            user_id: self.user_id.clone(),
            name: self.name.clone(),
            avatar_url: self.avatar_url.clone(),
            muted: self.muted,
            screen: self.screen,
        }
    }

    pub fn send(&self, message: ServerMsg) {
        let _ = self.out.send(message);
    }

    /// True once a disconnect has outlasted the grace period.
    pub fn is_abandoned(&self, now: Instant) -> bool {
        self.disconnected_since
            .map(|since| now.duration_since(since) >= ICE_DISCONNECT_GRACE)
            .unwrap_or(false)
    }

    pub fn is_speaking(&self, now: Instant) -> bool {
        self.speaking_until.map(|until| until > now).unwrap_or(false)
    }

    pub fn accepts(&self, input: &Input) -> bool {
        self.rtc.accepts(input)
    }

    pub fn handle_input(&mut self, input: Input) {
        if !self.rtc.is_alive() {
            return;
        }
        if let Err(error) = self.rtc.handle_input(input) {
            tracing::warn!(peer = self.id.0, %error, "peer input failed");
            self.rtc.disconnect();
        }
    }

    pub fn poll_output(&mut self, socket: &UdpSocket) -> Propagated {
        if !self.rtc.is_alive() {
            return Propagated::Noop;
        }
        // New subscriptions have to be negotiated before any media can flow to
        // them, and an offer must settle before the next one is made.
        if self.negotiate_if_needed() {
            return Propagated::Noop;
        }
        match self.rtc.poll_output() {
            Ok(output) => self.handle_output(output, socket),
            Err(error) => {
                tracing::warn!(peer = self.id.0, %error, "peer output failed");
                self.rtc.disconnect();
                Propagated::Noop
            }
        }
    }

    fn handle_output(&mut self, output: Output, socket: &UdpSocket) -> Propagated {
        match output {
            Output::Transmit(transmit) => {
                if let Err(error) = socket.send_to(&transmit.contents, transmit.destination) {
                    tracing::debug!(peer = self.id.0, %error, "udp send failed");
                }
                Propagated::Noop
            }
            Output::Timeout(at) => Propagated::Timeout(at),
            Output::Event(event) => self.handle_event(event),
        }
    }

    fn handle_event(&mut self, event: Event) -> Propagated {
        match event {
            Event::IceConnectionStateChange(state) => {
                match state {
                    IceConnectionState::Connected | IceConnectionState::Completed => {
                        self.connected = true;
                        if self.disconnected_since.take().is_some() {
                            tracing::info!(peer = self.id.0, user = %self.user_id, "ice recovered");
                        }
                    }
                    // Transient by design: hold the peer and let it come back.
                    // Only `reap` gives up, once the grace period is spent.
                    IceConnectionState::Disconnected => {
                        self.disconnected_since.get_or_insert_with(Instant::now);
                    }
                    _ => {}
                }
                tracing::info!(peer = self.id.0, user = %self.user_id, ?state, "ice state");
                Propagated::Noop
            }
            Event::MediaAdded(added) => self.handle_media_added(added),
            Event::MediaData(data) => self.handle_media_data_in(data),
            Event::KeyframeRequest(request) => self.handle_incoming_keyframe_request(request),
            _ => Propagated::Noop,
        }
    }
}
