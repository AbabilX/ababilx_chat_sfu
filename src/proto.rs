use serde::{Deserialize, Serialize};

/// Which peer a negotiated m-line carries. The browser cannot work this out
/// from SDP alone, so every offer states it.
#[derive(Debug, Clone, Serialize)]
pub struct TrackMap {
    pub mid: String,
    pub peer_id: String,
    /// "audio" or "video". Video is always the screen share.
    pub kind: String,
}

/// Who is in the room, as far as the app layer cares. Media state (speaking,
/// track presence) rides separately so this can be sent once on join.
#[derive(Debug, Clone, Serialize)]
pub struct PeerInfo {
    pub peer_id: String,
    pub user_id: String,
    pub name: String,
    pub avatar_url: String,
    pub muted: bool,
    pub screen: bool,
}

/// Browser -> SFU. The first frame must be `join`; anything else closes the
/// socket. Only the SFU offers after that, so there is no glare to resolve.
#[derive(Debug, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMsg {
    Join { token: String, sdp: String },
    Answer { sdp: String },
    Mute { muted: bool },
    Screen { active: bool },
    Leave,
    Ping,
}

/// SFU -> browser.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    Joined {
        peer_id: String,
        call_id: String,
        sdp: String,
        peers: Vec<PeerInfo>,
    },
    /// A renegotiation offer. Always answered, never counter-offered.
    Offer { sdp: String, tracks: Vec<TrackMap> },
    PeerJoined { peer: PeerInfo },
    PeerLeft { peer_id: String },
    PeerState { peer_id: String, muted: bool, screen: bool },
    /// Peer ids currently talking, recomputed from the RTP audio-level header
    /// extension. The SFU never decodes audio to work this out.
    Speaking { peer_ids: Vec<String> },
    Error { code: String, message: String },
    Pong,
}

impl ServerMsg {
    pub fn error(code: &str, message: &str) -> Self {
        ServerMsg::Error { code: code.to_string(), message: message.to_string() }
    }
}
