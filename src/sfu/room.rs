use std::time::Instant;

use crate::proto::{PeerInfo, ServerMsg};

use super::peer::Peer;
use super::tracks::{PeerId, Propagated, TrackOut, TrackOutState};

/// A call. Rooms are created by the first join and destroyed by the last leave;
/// nothing else can bring one into existence, so a stale room cannot linger.
pub struct Room {
    /// The host application's identifier for this call, carried for logs and
    /// for anything that needs to correlate a room with its own records.
    pub call_id: String,
    pub peers: Vec<Peer>,
    /// Last speaking set broadcast, so an unchanged set costs nothing.
    speaking: Vec<String>,
}

impl Room {
    pub fn new(call_id: String) -> Self {
        Room { call_id, peers: Vec::new(), speaking: Vec::new() }
    }

    pub fn peer_mut(&mut self, id: PeerId) -> Option<&mut Peer> {
        self.peers.iter_mut().find(|peer| peer.id == id)
    }

    pub fn roster(&self) -> Vec<PeerInfo> {
        self.peers.iter().map(Peer::info).collect()
    }

    pub fn broadcast(&self, message: ServerMsg) {
        for peer in &self.peers {
            peer.send(message.clone());
        }
    }

    /// Hands one peer's output to everyone else in the room.
    pub fn propagate(&mut self, propagated: &Propagated) {
        let Some(origin) = propagated.source() else { return };
        for peer in self.peers.iter_mut() {
            if peer.id == origin {
                continue;
            }
            match propagated {
                Propagated::TrackOpen(_, track_in) => {
                    peer.tracks_out.push(TrackOut {
                        track_in: track_in.clone(),
                        state: TrackOutState::ToOpen,
                    });
                }
                Propagated::Media(_, data) => peer.forward(origin, data),
                Propagated::KeyframeRequest(_, request, publisher, mid_in) => {
                    if *publisher == peer.id {
                        peer.serve_keyframe_request(*mid_in, request.kind);
                    }
                }
                Propagated::Noop | Propagated::Timeout(_) => {}
            }
        }
    }

    /// Gives a joining peer a subscription to everything already being published.
    pub fn subscribe_to_existing(&mut self, joining: PeerId) {
        let existing: Vec<_> = self
            .peers
            .iter()
            .filter(|peer| peer.id != joining)
            .flat_map(|peer| peer.tracks_in.iter().map(|track| std::sync::Arc::downgrade(&track.id)))
            .collect();
        let Some(peer) = self.peer_mut(joining) else { return };
        for track_in in existing {
            peer.tracks_out.push(TrackOut { track_in, state: TrackOutState::ToOpen });
        }
    }

    /// Recomputes who is talking from the audio-level header extension and
    /// broadcasts only when the set actually moves.
    pub fn sync_speaking(&mut self, now: Instant) {
        let mut current: Vec<String> = self
            .peers
            .iter()
            .filter(|peer| !peer.muted && peer.is_speaking(now))
            .map(|peer| peer.id.as_string())
            .collect();
        current.sort();
        if current == self.speaking {
            return;
        }
        self.speaking = current.clone();
        self.broadcast(ServerMsg::Speaking { peer_ids: current });
    }

    /// Removes a peer and tells the room. Returns it so the caller can decide
    /// whether the departure is worth reporting to the host application.
    pub fn remove(&mut self, id: PeerId) -> Option<Peer> {
        let index = self.peers.iter().position(|peer| peer.id == id)?;
        let peer = self.peers.remove(index);
        self.speaking.retain(|entry| entry != &id.as_string());
        self.broadcast(ServerMsg::PeerLeft { peer_id: id.as_string() });
        Some(peer)
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }
}
