use std::sync::mpsc::TryRecvError;

use crate::proto::ServerMsg;
use crate::report::ReportEvent;

use super::command::Command;
use super::engine::{ControlFlow, Engine};
use super::tracks::PeerId;

impl Engine {
    pub(super) fn drain_commands(&mut self) -> ControlFlow {
        loop {
            match self.rx.try_recv() {
                Ok(Command::Shutdown) | Err(TryRecvError::Disconnected) => {
                    return ControlFlow::Stop
                }
                Ok(command) => self.apply(command),
                Err(TryRecvError::Empty) => return ControlFlow::Continue,
            }
        }
    }

    fn apply(&mut self, command: Command) {
        match command {
            Command::Join(request) => self.handle_join(*request),
            Command::Answer { peer, sdp } => self.handle_answer(peer, sdp),
            Command::Mute { peer, muted } => self.set_state(peer, Some(muted), None),
            Command::Screen { peer, active } => self.set_state(peer, None, Some(active)),
            Command::Leave { peer } => self.drop_peer(peer, "left"),
            Command::Shutdown => {}
        }
    }

    fn handle_answer(&mut self, id: PeerId, sdp: String) {
        let Some(room) = self.room_of_mut(id) else { return };
        let Some(peer) = room.peer_mut(id) else { return };
        match str0m::change::SdpAnswer::from_sdp_string(&sdp) {
            Ok(answer) => {
                if let Err(error) = peer.accept_answer(answer) {
                    tracing::warn!(peer = id.0, %error, "answer rejected");
                }
            }
            Err(error) => tracing::warn!(peer = id.0, %error, "answer unparseable"),
        }
    }

    /// Mute and screen-share are application state, not media state. The SFU
    /// keeps them only so a joiner sees the room as it already is, and so the
    /// host application learns who is sharing.
    fn set_state(&mut self, id: PeerId, muted: Option<bool>, screen: Option<bool>) {
        let Some(room_name) = self.peer_rooms.get(&id).cloned() else { return };
        let Some(room) = self.rooms.get_mut(&room_name) else { return };
        let Some(peer) = room.peer_mut(id) else { return };
        if let Some(muted) = muted {
            peer.muted = muted;
        }
        let mut screen_changed = None;
        if let Some(active) = screen {
            if peer.screen != active {
                peer.screen = active;
                screen_changed = Some((peer.user_id.clone(), active));
            }
        }
        let state = ServerMsg::PeerState {
            peer_id: id.as_string(),
            muted: peer.muted,
            screen: peer.screen,
        };
        room.broadcast(state);
        if let Some((user_id, active)) = screen_changed {
            let event = if active { "track_published" } else { "track_unpublished" };
            self.report(
                ReportEvent::new(event, &room_name)
                    .identity(&user_id)
                    .screen_share(),
            );
        }
    }
}
