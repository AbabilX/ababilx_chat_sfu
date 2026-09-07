use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;
use tokio::sync::mpsc::UnboundedReceiver;

/// Room lifecycle reported to the host application. The field names match
/// LiveKit's webhook payload on purpose: an existing LiveKit integration can
/// point at this SFU and keep its handler, its event de-duplication and its
/// billing untouched. Only the authentication scheme differs — an HMAC over the
/// exact bytes posted, rather than a signed JWT.
#[derive(Debug, Clone, Serialize)]
pub struct ReportEvent {
    pub id: String,
    pub event: &'static str,
    pub room: ReportRoom,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub participant: Option<ReportParticipant>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track: Option<ReportTrack>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReportRoom {
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReportParticipant {
    pub identity: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReportTrack {
    pub source: &'static str,
}

pub const SCREEN_SHARE: &str = "SCREEN_SHARE";

/// Base64 HMAC-SHA256 of the raw request body, keyed by the webhook secret.
pub const SIGNATURE_HEADER: &str = "x-sfu-signature";

/// Announced once on boot: every room this process was carrying is gone.
pub const SERVER_STARTED: &str = "sfu_started";

impl ReportEvent {
    pub fn new(event: &'static str, room: &str) -> Self {
        ReportEvent {
            id: uuid::Uuid::new_v4().to_string(),
            event,
            room: ReportRoom { name: room.to_string() },
            participant: None,
            track: None,
        }
    }

    pub fn identity(mut self, identity: &str) -> Self {
        self.participant = Some(ReportParticipant { identity: identity.to_string() });
        self
    }

    pub fn screen_share(mut self) -> Self {
        self.track = Some(ReportTrack { source: SCREEN_SHARE });
        self
    }
}

/// Signs the exact bytes that are posted. The API recomputes the MAC over the
/// raw body, so this must stay byte-identical to what reqwest sends — hence
/// serializing once and posting the buffer, not the struct.
pub fn sign(body: &[u8], secret: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .expect("hmac accepts any key length");
    mac.update(body);
    STANDARD.encode(mac.finalize().into_bytes())
}

/// Drains reports to the API. Delivery is at-least-once and the API dedups on
/// `id`, so a retry is always safe. A report that will not go through is logged
/// and dropped: losing one must never wedge the media loop.
pub async fn run(mut rx: UnboundedReceiver<ReportEvent>, url: String, secret: String) {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("reqwest client");
    while let Some(event) = rx.recv().await {
        let Ok(body) = serde_json::to_vec(&event) else { continue };
        let signature = sign(&body, &secret);
        // The boot announcement is the one report that must not be lost: it is
        // what releases participants from rooms that died with the previous
        // process, and the host application may still be starting up itself.
        let attempts: u32 = if event.event == SERVER_STARTED { 12 } else { 3 };
        let mut delivered = false;
        for attempt in 0..attempts {
            let result = client
                .post(&url)
                .header("content-type", "application/json")
                .header(SIGNATURE_HEADER, &signature)
                .body(body.clone())
                .send()
                .await;
            match result {
                Ok(response) if response.status().is_success() => {
                    delivered = true;
                    break;
                }
                Ok(response) => {
                    tracing::warn!(event = event.event, status = %response.status(), "report rejected");
                }
                Err(error) => {
                    tracing::warn!(event = event.event, %error, "report failed");
                }
            }
            let backoff = (250 * (attempt as u64 + 1)).min(5_000);
            tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
        }
        if !delivered {
            tracing::error!(event = event.event, room = %event.room.name, "report dropped");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_is_stable_and_key_dependent() {
        assert_eq!(sign(b"{}", "a"), sign(b"{}", "a"));
        assert_ne!(sign(b"{}", "a"), sign(b"{}", "b"));
        assert_ne!(sign(b"{}", "a"), sign(b"{ }", "a"));
    }

    #[test]
    fn events_carry_the_livekit_field_names() {
        let event = ReportEvent::new("participant_joined", "room-1").identity("user-1");
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["event"], "participant_joined");
        assert_eq!(json["room"]["name"], "room-1");
        assert_eq!(json["participant"]["identity"], "user-1");
        assert!(json.get("track").is_none());
    }
}
