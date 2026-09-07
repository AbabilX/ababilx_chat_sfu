use std::collections::VecDeque;
use std::io::ErrorKind;
use std::time::Instant;

use str0m::net::{Protocol, Receive};
use str0m::Input;

use crate::report::ReportEvent;

use super::engine::{Engine, MAX_IDLE, SPEAKING_TICK};
use super::tracks::Propagated;

impl Engine {
    /// Drains every peer's output, sending packets as they appear and queueing
    /// anything the rest of the room needs. Returns the earliest wake-up any
    /// peer asked for.
    pub(super) fn poll_rooms(&mut self) -> Instant {
        let mut timeout = Instant::now() + MAX_IDLE;
        let socket = &self.socket;
        for room in self.rooms.values_mut() {
            let mut queue: VecDeque<Propagated> = VecDeque::new();
            for peer in room.peers.iter_mut() {
                loop {
                    if !peer.rtc.is_alive() {
                        break;
                    }
                    let propagated = peer.poll_output(socket);
                    if let Propagated::Timeout(at) = propagated {
                        timeout = timeout.min(at);
                        break;
                    }
                    queue.push_back(propagated);
                }
            }
            // Propagation is deferred so the borrow of the producing peer has
            // ended before its subscribers are touched.
            while let Some(propagated) = queue.pop_front() {
                room.propagate(&propagated);
            }
        }
        timeout
    }

    /// Announces peers whose transport has just come up. Reported here rather
    /// than at join so an arrival means "media can flow", which is what the
    /// host application's call clock and participant peak actually mean.
    pub(super) fn report_connections(&mut self) {
        let mut arrived: Vec<(String, String)> = Vec::new();
        for (room_name, room) in self.rooms.iter_mut() {
            for peer in room.peers.iter_mut() {
                if peer.connected && !peer.reported {
                    peer.reported = true;
                    arrived.push((room_name.clone(), peer.user_id.clone()));
                }
            }
        }
        for (room_name, user_id) in arrived {
            tracing::info!(user = %user_id, room = %room_name, "peer connected");
            self.report(ReportEvent::new("participant_joined", &room_name).identity(&user_id));
        }
    }

    pub(super) fn tick_speaking(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_speaking) < SPEAKING_TICK {
            return;
        }
        self.last_speaking = now;
        for room in self.rooms.values_mut() {
            room.sync_speaking(now);
        }
    }

    /// Reads one datagram. Every peer in every room is multiplexed over this one
    /// socket; str0m's own `accepts()` is the demultiplexer, because it is what
    /// knows which instance owns an ICE ufrag or a DTLS association.
    pub(super) fn read_once(&mut self, buf: &mut Vec<u8>) {
        buf.resize(2000, 0);
        let (len, source) = match self.socket.recv_from(buf) {
            Ok(value) => value,
            Err(error) => {
                if !matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) {
                    tracing::warn!(%error, "udp read failed");
                }
                return;
            }
        };
        buf.truncate(len);
        let Ok(contents) = buf.as_slice().try_into() else { return };
        // The destination must be the address we ADVERTISED, not the socket's
        // own. str0m matches an incoming packet to a local ICE candidate by
        // this address, and a server binds a wildcard (0.0.0.0) while
        // advertising one routable IP — reporting 0.0.0.0 here matches no
        // candidate, so no pair ever validates and ICE silently never connects.
        let destination = self.advertised;
        let input = Input::Receive(
            Instant::now(),
            Receive { proto: Protocol::Udp, source, destination, contents },
        );
        let target = self
            .rooms
            .values_mut()
            .flat_map(|room| room.peers.iter_mut())
            .find(|peer| peer.accepts(&input));
        match target {
            Some(peer) => peer.handle_input(input),
            // Routine: a browser STUNs before its join command has been applied.
            None => tracing::trace!(%source, "no peer accepted packet"),
        }
    }
}
