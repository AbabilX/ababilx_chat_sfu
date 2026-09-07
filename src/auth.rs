use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use serde::Deserialize;

/// The join token the Go API mints. It is deliberately NOT LiveKit-shaped: the
/// SFU trusts exactly these fields and nothing else, and the room name is the
/// `provider_room_name` already stored on `group_call_rooms`.
#[derive(Debug, Clone, Deserialize)]
pub struct JoinClaims {
    /// The user id. Doubles as the participant identity the API expects back in
    /// its webhook events.
    pub sub: String,
    pub room: String,
    pub call_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub avatar_url: String,
    /// Read by the JWT library, not by us — the field must exist for expiry to
    /// be enforced.
    #[allow(dead_code)]
    pub exp: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// The library refused it. The kind is carried because a wrong shared
    /// secret, an expired token and a truncated one are three different
    /// operator mistakes, and collapsing them into one message meant a
    /// misconfigured deployment looked exactly like a clock skew.
    #[error("token invalid ({0:?})")]
    Invalid(jsonwebtoken::errors::ErrorKind),
    #[error("token is missing sub, room or call_id")]
    IncompleteClaims,
}

/// Verifies a join token. Expiry is enforced by the library; a 90s TTL is minted
/// by the API so a leaked token is useless almost immediately.
pub fn verify(token: &str, secret: &str) -> Result<JoinClaims, AuthError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_aud = false;
    validation.set_required_spec_claims(&["exp"]);
    decode::<JoinClaims>(
        token.trim(),
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .map(|data| data.claims)
    .map_err(|error| AuthError::Invalid(error.into_kind()))
    .and_then(|claims| {
        if claims.sub.is_empty() || claims.room.is_empty() || claims.call_id.is_empty() {
            Err(AuthError::IncompleteClaims)
        } else {
            Ok(claims)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{encode, EncodingKey, Header};
    use serde_json::json;

    fn mint(secret: &str, exp: i64) -> String {
        encode(
            &Header::new(Algorithm::HS256),
            &json!({"sub": "u1", "room": "r1", "call_id": "c1", "name": "A", "exp": exp}),
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap()
    }

    fn soon() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 90
    }

    #[test]
    fn accepts_a_token_minted_with_the_shared_secret() {
        let claims = verify(&mint("s3cret", soon()), "s3cret").expect("valid");
        assert_eq!(claims.sub, "u1");
        assert_eq!(claims.room, "r1");
        assert_eq!(claims.call_id, "c1");
    }

    #[test]
    fn rejects_a_different_secret() {
        assert!(verify(&mint("s3cret", soon()), "other").is_err());
    }

    #[test]
    fn rejects_an_expired_token() {
        assert!(verify(&mint("s3cret", 1_000), "s3cret").is_err());
    }

    /// The whole point of carrying the kind: these two must not look alike in a
    /// log, because they are fixed in completely different places.
    #[test]
    fn distinguishes_a_wrong_secret_from_an_expired_token() {
        let wrong_secret = verify(&mint("s3cret", soon()), "other").unwrap_err();
        let expired = verify(&mint("s3cret", 1_000), "s3cret").unwrap_err();
        assert!(matches!(
            wrong_secret,
            AuthError::Invalid(jsonwebtoken::errors::ErrorKind::InvalidSignature)
        ));
        assert!(matches!(
            expired,
            AuthError::Invalid(jsonwebtoken::errors::ErrorKind::ExpiredSignature)
        ));
    }
}
