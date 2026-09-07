use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use crate::auth::JoinClaims;
use crate::proto::ServerMsg;

use super::tracks::PeerId;

/// A join, carried whole so the media loop can accept or refuse it without
/// touching the socket task again.
pub struct JoinRequest {
    pub claims: JoinClaims,
    /// The browser's one and only offer: its microphone plus a video m-line
    /// held ready for a screen share.
    pub sdp: String,
    pub out: UnboundedSender<ServerMsg>,
    /// Answered with the assigned peer id, or None when the join is refused.
    pub assigned: oneshot::Sender<Option<PeerId>>,
}

/// Everything the signaling tasks can ask of the media loop. The loop owns all
/// WebRTC state and is single-threaded, so these are its only mutations.
pub enum Command {
    Join(Box<JoinRequest>),
    Answer { peer: PeerId, sdp: String },
    Mute { peer: PeerId, muted: bool },
    Screen { peer: PeerId, active: bool },
    Leave { peer: PeerId },
    Shutdown,
}
