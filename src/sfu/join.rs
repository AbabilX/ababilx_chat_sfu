use std::time::Instant;

use str0m::{Candidate, Rtc};

use crate::proto::ServerMsg;
use crate::report::ReportEvent;

use super::command::JoinRequest;
use super::engine::Engine;
use super::peer::Peer;
use super::room::Room;
use super::tracks::PeerId;

impl Engine {
    pub(super) fn handle_join(&mut self, request: JoinRequest) {
        let JoinRequest { claims, sdp, out, assigned } = request;
        let room_name = claims.room.clone();
        let full = self
            .rooms
            .get(&room_name)
            .map(|room| room.peers.len() >= self.config.max_room_peers)
            .unwrap_or(false);
        if full {
            tracing::warn!(
                room = %room_name, user = %claims.sub, max = self.config.max_room_peers,
                "join refused: room full"
            );
            let _ = out.send(ServerMsg::error("room_full", "This call is full."));
            let _ = assigned.send(None);
            return;
        }
        let offer = match str0m::change::SdpOffer::from_sdp_string(&sdp) {
            Ok(offer) => offer,
            Err(error) => {
                tracing::warn!(%error, "join offer unparseable");
                let _ = out.send(ServerMsg::error("bad_offer", "Malformed session description."));
                let _ = assigned.send(None);
                return;
            }
        };

        let id = PeerId(self.next_peer);
        self.next_peer += 1;
        let mut rtc = Rtc::builder()
            .set_ice_lite(self.config.ice_lite)
            .build(Instant::now());
        // One shared UDP port for every room, announced as the box's own public
        // address. A browser needs a routable candidate; 127.0.0.1 is refused.
        match Candidate::host(self.advertised, "udp") {
            Ok(candidate) => {
                if rtc.add_local_candidate(candidate).is_none() {
                    tracing::error!(addr = %self.advertised, "local candidate rejected");
                }
            }
            Err(error) => tracing::error!(%error, "cannot build host candidate"),
        }

        let mut peer = Peer::new(
            id,
            rtc,
            claims.sub.clone(),
            claims.name.clone(),
            claims.avatar_url.clone(),
            out,
        );
        let answer = match peer.accept_offer(offer) {
            Ok(answer) => answer,
            Err(error) => {
                tracing::warn!(%error, "join offer refused");
                peer.send(ServerMsg::error("bad_offer", "Session description refused."));
                let _ = assigned.send(None);
                return;
            }
        };

        let room = self
            .rooms
            .entry(room_name.clone())
            .or_insert_with(|| Room::new(claims.call_id.clone()));
        // A user reconnecting is a new peer. Retire the old one WITHOUT
        // reporting a departure: the host application keys participants by
        // identity, so a late "left" would erase the fresh arrival.
        let stale: Vec<PeerId> = room
            .peers
            .iter()
            .filter(|existing| existing.user_id == claims.sub)
            .map(|existing| existing.id)
            .collect();
        for old in stale {
            if let Some(mut previous) = room.remove(old) {
                previous.reported = false;
                previous.rtc.disconnect();
                previous.send(ServerMsg::error("replaced", "Joined from another tab."));
            }
            self.peer_rooms.remove(&old);
        }

        let info = peer.info();
        peer.send(ServerMsg::Joined {
            peer_id: id.as_string(),
            call_id: claims.call_id.clone(),
            sdp: answer,
            peers: room.roster(),
        });
        room.broadcast(ServerMsg::PeerJoined { peer: info });
        room.peers.push(peer);
        room.subscribe_to_existing(id);
        self.peer_rooms.insert(id, room_name.clone());
        let _ = assigned.send(Some(id));
        tracing::info!(peer = id.0, user = %claims.sub, room = %room_name, "peer joined");
        // No arrival is reported yet. A socket that never completes ICE never
        // carried media, and telling the host application otherwise would start
        // a call clock for a participant who never showed up.
    }

    /// Removes a peer and, when that empties the room, tears the room down.
    pub(super) fn drop_peer(&mut self, id: PeerId, reason: &str) {
        let Some(room_name) = self.peer_rooms.remove(&id) else { return };
        let (reported, user_id, emptied, call_id) = {
            let Some(room) = self.rooms.get_mut(&room_name) else { return };
            let Some(mut peer) = room.remove(id) else { return };
            peer.rtc.disconnect();
            (peer.reported, peer.user_id.clone(), room.is_empty(), room.call_id.clone())
        };
        tracing::info!(peer = id.0, user = %user_id, reason, "peer left");
        if reported {
            self.report(ReportEvent::new("participant_left", &room_name).identity(&user_id));
        }
        if emptied {
            self.rooms.remove(&room_name);
            tracing::info!(room = %room_name, call = %call_id, "room finished");
            self.report(ReportEvent::new("room_finished", &room_name));
        }
    }

    /// Sweeps peers whose transport died without a clean leave — a closed
    /// laptop, a dropped network, a killed tab — and those whose ICE has stayed
    /// disconnected past the point of recovering.
    pub(super) fn reap(&mut self) {
        let now = std::time::Instant::now();
        let dead: Vec<(PeerId, &'static str)> = self
            .rooms
            .values()
            .flat_map(|room| room.peers.iter())
            .filter_map(|peer| {
                if !peer.rtc.is_alive() {
                    Some((peer.id, "transport closed"))
                } else if peer.is_abandoned(now) {
                    Some((peer.id, "ice disconnected"))
                } else {
                    None
                }
            })
            .collect();
        for (id, reason) in dead {
            self.drop_peer(id, reason);
        }
    }
}
