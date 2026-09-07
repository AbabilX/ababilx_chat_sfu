use str0m::change::{SdpAnswer, SdpOffer};
use str0m::media::{Direction, MediaKind};

use crate::proto::{ServerMsg, TrackMap};

use super::peer::Peer;
use super::tracks::{TrackOutState, TrackOut};

impl Peer {
    /// Brings this peer's subscriptions in line with the room and, if anything
    /// changed, offers. Only the SFU ever offers after the initial exchange, so
    /// there is no glare to resolve on either side.
    ///
    /// Returns true when an offer went out; the caller must then leave this peer
    /// alone until the answer lands.
    pub(super) fn negotiate_if_needed(&mut self) -> bool {
        if self.pending.is_some() || !self.rtc.is_alive() {
            return false;
        }
        // A publisher that left leaves its subscribers holding a dead Weak. Stop
        // the m-line so the browser's transceiver goes to "stopped" instead of
        // sitting there forever waiting for packets.
        for track in &mut self.tracks_out {
            if let TrackOutState::Open(mid) = track.state {
                if track.track_in.upgrade().is_none() {
                    track.state = TrackOutState::ToStop(mid);
                }
            }
        }

        let mut change = self.rtc.sdp_api();
        for track in &mut self.tracks_out {
            match track.state {
                TrackOutState::ToOpen => {
                    if let Some(track_in) = track.track_in.upgrade() {
                        let stream_id = track_in.origin.as_string();
                        let track_id = format!("{stream_id}-{}", kind_label(track_in.kind));
                        let mid = change.add_media(
                            track_in.kind,
                            Direction::SendOnly,
                            Some(stream_id),
                            Some(track_id),
                            None,
                        );
                        track.state = TrackOutState::Negotiating(mid);
                    }
                }
                TrackOutState::ToStop(mid) => {
                    change.stop_media(mid);
                    track.state = TrackOutState::NegotiatingStop(mid);
                }
                _ => {}
            }
        }
        if !change.has_changes() {
            return false;
        }
        let Some((offer, pending)) = change.apply() else {
            return false;
        };
        let tracks = track_map(&self.tracks_out);
        self.pending = Some(pending);
        self.send(ServerMsg::Offer { sdp: offer.to_sdp_string(), tracks });
        true
    }

    /// Applies the answer to an offer we sent. Anything mid-negotiation becomes
    /// live; anything mid-stop is dropped so its m-line can be recycled.
    pub(super) fn accept_answer(&mut self, answer: SdpAnswer) -> Result<(), String> {
        let Some(pending) = self.pending.take() else {
            return Err("no_pending_offer".to_string());
        };
        self.rtc
            .sdp_api()
            .accept_answer(pending, answer)
            .map_err(|error| error.to_string())?;
        for track in &mut self.tracks_out {
            if let TrackOutState::Negotiating(mid) = track.state {
                track.state = TrackOutState::Open(mid);
            }
        }
        self.tracks_out
            .retain(|track| !matches!(track.state, TrackOutState::NegotiatingStop(_)));
        Ok(())
    }

    /// Answers the peer's one and only offer, sent when it joins.
    pub(super) fn accept_offer(&mut self, offer: SdpOffer) -> Result<String, String> {
        self.rtc
            .sdp_api()
            .accept_offer(offer)
            .map(|answer| answer.to_sdp_string())
            .map_err(|error| error.to_string())
    }
}

/// Every outbound m-line this peer currently has, so the browser can attribute
/// an incoming track to a person. SDP alone does not say whose audio a new
/// m-line carries.
pub(super) fn track_map(tracks_out: &[TrackOut]) -> Vec<TrackMap> {
    tracks_out
        .iter()
        .filter_map(|track| {
            let mid = track.mid()?;
            let track_in = track.track_in.upgrade()?;
            if matches!(
                track.state,
                TrackOutState::ToStop(_) | TrackOutState::NegotiatingStop(_)
            ) {
                return None;
            }
            Some(TrackMap {
                mid: mid.to_string(),
                peer_id: track_in.origin.as_string(),
                kind: kind_label(track_in.kind).to_string(),
            })
        })
        .collect()
}

fn kind_label(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Audio => "audio",
        MediaKind::Video => "video",
    }
}
