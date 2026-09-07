use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use str0m::Input;
use tokio::sync::mpsc::UnboundedSender;

use crate::config::Config;
use crate::report::ReportEvent;

use super::command::Command;
use super::room::Room;
use super::tracks::PeerId;

/// Ceiling on how long the loop sleeps waiting for a packet. str0m usually asks
/// for something shorter; this only bounds the idle case.
pub(super) const MAX_IDLE: Duration = Duration::from_millis(100);
pub(super) const SPEAKING_TICK: Duration = Duration::from_millis(200);

/// Handle used by the signaling tasks to talk to the media loop.
#[derive(Clone)]
pub struct EngineHandle {
    tx: Sender<Command>,
}

impl EngineHandle {
    pub fn send(&self, command: Command) -> bool {
        self.tx.send(command).is_ok()
    }
}

/// Owns every `Rtc` instance and the one media socket. Single-threaded on
/// purpose: all mutation arrives as a `Command`, so there are no locks on the
/// packet path.
pub struct Engine {
    pub(super) config: Arc<Config>,
    pub(super) socket: UdpSocket,
    pub(super) advertised: SocketAddr,
    pub(super) rooms: HashMap<String, Room>,
    pub(super) peer_rooms: HashMap<PeerId, String>,
    pub(super) next_peer: u64,
    pub(super) reports: UnboundedSender<ReportEvent>,
    pub(super) rx: Receiver<Command>,
    pub(super) last_speaking: Instant,
}

/// Binds the media socket and starts the loop on its own OS thread. str0m is
/// synchronous by design, so it gets a thread rather than competing with the
/// async runtime that serves signaling.
pub fn spawn(
    config: Arc<Config>,
    reports: UnboundedSender<ReportEvent>,
) -> std::io::Result<(EngineHandle, SocketAddr)> {
    let socket = UdpSocket::bind(config.udp_bind)?;
    let bound = socket.local_addr()?;
    let advertised = config.advertised_udp(bound);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("sfu-media".to_string())
        .spawn(move || {
            Engine {
                config,
                socket,
                advertised,
                rooms: HashMap::new(),
                peer_rooms: HashMap::new(),
                next_peer: 1,
                reports,
                rx,
                last_speaking: Instant::now(),
            }
            .run();
        })?;
    Ok((EngineHandle { tx }, advertised))
}

impl Engine {
    fn run(&mut self) {
        let mut buf = vec![0_u8; 2000];
        loop {
            if matches!(self.drain_commands(), ControlFlow::Stop) {
                tracing::info!("media loop stopping");
                return;
            }
            let timeout = self.poll_rooms();
            self.report_connections();
            self.reap();
            self.tick_speaking();
            let wait = timeout
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(1));
            if self.socket.set_read_timeout(Some(wait)).is_err() {
                return;
            }
            self.read_once(&mut buf);
            let now = Instant::now();
            for room in self.rooms.values_mut() {
                for peer in room.peers.iter_mut() {
                    peer.handle_input(Input::Timeout(now));
                }
            }
        }
    }

    pub(super) fn room_of_mut(&mut self, id: PeerId) -> Option<&mut Room> {
        let name = self.peer_rooms.get(&id)?.clone();
        self.rooms.get_mut(&name)
    }

    pub(super) fn report(&self, event: ReportEvent) {
        let _ = self.reports.send(event);
    }
}

pub(super) enum ControlFlow {
    Continue,
    Stop,
}
