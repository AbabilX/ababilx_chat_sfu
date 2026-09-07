use std::sync::{Arc, Weak};
use std::time::Instant;

use str0m::media::{KeyframeRequest, MediaData, MediaKind, Mid};

/// Identifies a peer inside the media loop. Monotonic and never reused, so a
/// user who reconnects is a genuinely different peer and cannot inherit the
/// dying one's tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PeerId(pub u64);

impl PeerId {
    pub fn as_string(&self) -> String {
        self.0.to_string()
    }
}

/// A track a peer is publishing to us.
#[derive(Debug)]
pub struct TrackIn {
    pub origin: PeerId,
    pub mid: Mid,
    pub kind: MediaKind,
}

#[derive(Debug)]
pub struct TrackInEntry {
    pub id: Arc<TrackIn>,
    pub last_keyframe_request: Option<Instant>,
}

/// One publisher's track, as forwarded to one subscriber. The `Weak` is what
/// makes a publisher leaving self-healing: the upgrade fails and the m-line is
/// stopped on the next negotiation.
#[derive(Debug)]
pub struct TrackOut {
    pub track_in: Weak<TrackIn>,
    pub state: TrackOutState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackOutState {
    ToOpen,
    Negotiating(Mid),
    Open(Mid),
    ToStop(Mid),
    NegotiatingStop(Mid),
}

impl TrackOut {
    pub fn mid(&self) -> Option<Mid> {
        match self.state {
            TrackOutState::ToOpen => None,
            TrackOutState::Negotiating(mid)
            | TrackOutState::Open(mid)
            | TrackOutState::ToStop(mid)
            | TrackOutState::NegotiatingStop(mid) => Some(mid),
        }
    }
}

/// Something one peer produced that the rest of its room may need. Kept as an
/// enum rather than direct calls so the borrow of the producing peer ends
/// before the subscribers are touched.
#[derive(Debug)]
pub enum Propagated {
    Noop,
    Timeout(Instant),
    TrackOpen(PeerId, Weak<TrackIn>),
    Media(PeerId, Box<MediaData>),
    KeyframeRequest(PeerId, KeyframeRequest, PeerId, Mid),
}

impl Propagated {
    /// The peer the event came from, when it is something other peers care about.
    pub fn source(&self) -> Option<PeerId> {
        match self {
            Propagated::TrackOpen(id, _)
            | Propagated::Media(id, _)
            | Propagated::KeyframeRequest(id, _, _, _) => Some(*id),
            Propagated::Noop | Propagated::Timeout(_) => None,
        }
    }
}
