use std::sync::Arc;
use std::time::{Duration, Instant};

use str0m::media::{KeyframeRequest, KeyframeRequestKind, MediaAdded, MediaData, MediaKind, Mid};

use super::peer::{Peer, SPEAKING_HOLD, SPEAKING_THRESHOLD_DBOV};
use super::tracks::{Propagated, TrackIn, TrackInEntry};

impl Peer {
    pub(super) fn handle_media_added(&mut self, added: MediaAdded) -> Propagated {
        let entry = TrackInEntry {
            id: Arc::new(TrackIn { origin: self.id, mid: added.mid, kind: added.kind }),
            last_keyframe_request: None,
        };
        let weak = Arc::downgrade(&entry.id);
        self.tracks_in.push(entry);
        tracing::debug!(peer = self.id.0, mid = %added.mid, kind = ?added.kind, "track published");
        Propagated::TrackOpen(self.id, weak)
    }

    pub(super) fn handle_media_data_in(&mut self, data: MediaData) -> Propagated {
        let kind = self
            .tracks_in
            .iter()
            .find(|track| track.id.mid == data.mid)
            .map(|track| track.id.kind);
        match kind {
            Some(MediaKind::Audio) => self.note_audio_level(&data),
            // A gap in video is only recoverable with a fresh keyframe, and the
            // publisher is the only one who can produce one.
            Some(MediaKind::Video) if !data.contiguous => {
                self.request_keyframe_throttled(data.mid, KeyframeRequestKind::Pli);
            }
            _ => {}
        }
        Propagated::Media(self.id, Box::new(data))
    }

    /// Reads the RFC 6464 audio-level header extension. It lives in the RTP
    /// header, NOT the payload, so active-speaker detection keeps working
    /// unchanged once the payload becomes end-to-end encrypted.
    fn note_audio_level(&mut self, data: &MediaData) {
        let Some(level) = data.ext_vals.audio_level else { return };
        let voiced = data.ext_vals.voice_activity.unwrap_or(true);
        if voiced && level > -SPEAKING_THRESHOLD_DBOV {
            self.speaking_until = Some(Instant::now() + SPEAKING_HOLD);
        }
    }

    pub(super) fn request_keyframe_throttled(&mut self, mid: Mid, kind: KeyframeRequestKind) {
        let Some(entry) = self.tracks_in.iter().find(|track| track.id.mid == mid) else {
            return;
        };
        if entry
            .last_keyframe_request
            .map(|at| at.elapsed() < Duration::from_secs(1))
            .unwrap_or(false)
        {
            return;
        }
        let Some(mut writer) = self.rtc.writer(mid) else { return };
        if writer.request_keyframe(None, kind).is_err() {
            return;
        }
        if let Some(entry) = self.tracks_in.iter_mut().find(|track| track.id.mid == mid) {
            entry.last_keyframe_request = Some(Instant::now());
        }
    }

    /// A subscriber asked for a keyframe on an m-line we forward. Translate it
    /// back to the publisher's own mid so the request reaches the real encoder.
    pub(super) fn handle_incoming_keyframe_request(
        &self,
        mut request: KeyframeRequest,
    ) -> Propagated {
        let Some(track_out) = self.tracks_out.iter().find(|t| t.mid() == Some(request.mid)) else {
            return Propagated::Noop;
        };
        let Some(track_in) = track_out.track_in.upgrade() else {
            return Propagated::Noop;
        };
        request.rid = None;
        Propagated::KeyframeRequest(self.id, request, track_in.origin, track_in.mid)
    }

    /// Writes one publisher's packet onto this subscriber's matching m-line.
    /// The payload is copied through untouched — the SFU never inspects it,
    /// which is what makes end-to-end encryption a client-only change later.
    pub(super) fn forward(&mut self, origin: super::tracks::PeerId, data: &MediaData) {
        let Some(mid) = self
            .tracks_out
            .iter()
            .find(|out| {
                out.track_in
                    .upgrade()
                    .filter(|track| track.origin == origin && track.mid == data.mid)
                    .is_some()
            })
            .and_then(|out| out.mid())
        else {
            return;
        };
        let Some(writer) = self.rtc.writer(mid) else { return };
        let Some(pt) = writer.match_params(data.params) else { return };
        let writer = match data.ext_vals.audio_level {
            Some(level) => {
                writer.audio_level(level, data.ext_vals.voice_activity.unwrap_or(true))
            }
            None => writer,
        };
        if let Err(error) = writer.write(pt, data.network_time, data.time, data.data.clone()) {
            tracing::warn!(peer = self.id.0, %error, "forward failed");
            self.rtc.disconnect();
        }
    }

    /// Passes a subscriber's keyframe request to the publisher's encoder.
    pub(super) fn serve_keyframe_request(&mut self, mid: Mid, kind: KeyframeRequestKind) {
        if !self.tracks_in.iter().any(|track| track.id.mid == mid) {
            return;
        }
        let Some(mut writer) = self.rtc.writer(mid) else { return };
        if let Err(error) = writer.request_keyframe(None, kind) {
            tracing::debug!(peer = self.id.0, %error, "keyframe request rejected");
        }
    }
}
