//! Spotify sign-in: OAuth 2.0 Authorization Code with PKCE and
//! the paste-back callback flow.
//!
//! Nothing here touches the network. The firmware supplies random bytes and
//! HTTP responses; these functions build URLs and bodies and read replies.

use alloc::format;
use alloc::string::{String, ToString};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::form_encode;

pub const AUTHORIZE_URL: &str = "https://accounts.spotify.com/authorize";
pub const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
/// Loopback redirect. Spotify allows plain HTTP only for loopback addresses.
/// Nothing listens there on the phone; the owner copies the URL back.
pub const REDIRECT_URI: &str = "http://127.0.0.1:8888/callback";
pub const SCOPES: &str =
    "user-read-currently-playing user-read-playback-state user-modify-playback-state";
/// Refresh the access token this long before it expires.
pub const REFRESH_MARGIN_S: u32 = 300;

/// A PKCE verifier and its S256 challenge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    /// Build from 32 random bytes, giving a 43-character verifier.
    pub fn from_random(random: &[u8; 32]) -> Self {
        let verifier = URL_SAFE_NO_PAD.encode(random);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self {
            verifier,
            challenge,
        }
    }
}

/// An opaque `state` value that ties the callback to this sign-in attempt.
pub fn state_from_random(random: &[u8; 16]) -> String {
    URL_SAFE_NO_PAD.encode(random)
}

/// The URL the **Connect Spotify** link opens.
pub fn authorize_url(client_id: &str, challenge: &str, state: &str) -> String {
    let query = form_encode(&[
        ("response_type", "code"),
        ("client_id", client_id),
        ("scope", SCOPES),
        ("redirect_uri", REDIRECT_URI),
        ("code_challenge_method", "S256"),
        ("code_challenge", challenge),
        ("state", state),
    ]);
    format!("{AUTHORIZE_URL}?{query}")
}

/// Why a pasted callback URL was rejected. Each one maps to a message in the
/// portal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallbackError {
    /// The paste doesn't look like the callback URL.
    NotACallback,
    /// The owner pressed Cancel on Spotify's page, or Spotify refused.
    Denied(String),
    /// `state` doesn't match: an old or foreign link. Start sign-in again.
    StateMismatch,
}

/// Pull the authorization `code` out of the URL the owner pasted.
pub fn parse_callback(pasted: &str, expected_state: &str) -> Result<String, CallbackError> {
    let pasted = pasted.trim();
    if !pasted.contains('?') && !pasted.starts_with("code=") && !pasted.starts_with("error=") {
        return Err(CallbackError::NotACallback);
    }

    let query = pasted.split_once('?').map_or(pasted, |(_, q)| q);
    let query = query.split('#').next().unwrap_or("");

    let mut code = None;
    let mut state = None;
    let mut error = None;
    for (key, value) in form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            _ => {}
        }
    }

    if let Some(error) = error {
        return Err(CallbackError::Denied(error));
    }
    if state.as_deref() != Some(expected_state) {
        return Err(CallbackError::StateMismatch);
    }
    code.filter(|c| !c.is_empty())
        .ok_or(CallbackError::NotACallback)
}

/// Form body for exchanging the authorization code for tokens.
pub fn code_exchange_body(client_id: &str, code: &str, verifier: &str) -> String {
    form_encode(&[
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", REDIRECT_URI),
        ("client_id", client_id),
        ("code_verifier", verifier),
    ])
}

/// Form body for refreshing the access token.
pub fn refresh_body(client_id: &str, refresh_token: &str) -> String {
    form_encode(&[
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client_id),
    ])
}

/// Tokens from a successful exchange or refresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tokens {
    pub access_token: String,
    pub expires_in_s: u32,
    /// Present when Spotify rotates the refresh token. Save it.
    pub refresh_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenError {
    /// The code expired or the refresh token was revoked. The owner needs to
    /// sign in again.
    InvalidGrant,
    /// The Client ID is wrong, or the Spotify app was deleted.
    InvalidClient,
    /// Any other OAuth error code.
    Other(String),
    /// Not an OAuth reply at all (outage page, truncated body, …). Retry later.
    Malformed { status: u16 },
}

#[derive(Deserialize)]
struct RawTokens {
    access_token: String,
    expires_in: u32,
    refresh_token: Option<String>,
}

#[derive(Deserialize)]
struct RawTokenError {
    error: String,
}

/// Read the token endpoint's reply.
pub fn parse_token_response(status: u16, body: &[u8]) -> Result<Tokens, TokenError> {
    if status == 200 {
        let raw: RawTokens =
            serde_json::from_slice(body).map_err(|_| TokenError::Malformed { status })?;
        return Ok(Tokens {
            access_token: raw.access_token,
            expires_in_s: raw.expires_in,
            refresh_token: raw.refresh_token,
        });
    }
    let raw: RawTokenError =
        serde_json::from_slice(body).map_err(|_| TokenError::Malformed { status })?;
    Err(match raw.error.as_str() {
        "invalid_grant" => TokenError::InvalidGrant,
        "invalid_client" => TokenError::InvalidClient,
        other => TokenError::Other(other.to_string()),
    })
}

/// Whether to refresh now, given when the token was issued (seconds, any
/// monotonic clock) and its lifetime.
pub fn needs_refresh(now_s: u64, issued_at_s: u64, expires_in_s: u32) -> bool {
    let refresh_at = issued_at_s + expires_in_s.saturating_sub(REFRESH_MARGIN_S) as u64;
    now_s >= refresh_at
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7636 Appendix B test vector.
    const RFC_RANDOM: [u8; 32] = [
        116, 24, 223, 180, 151, 153, 224, 37, 79, 250, 96, 125, 216, 173, 187, 186, 22, 212, 37,
        77, 105, 214, 191, 240, 91, 88, 5, 88, 83, 132, 141, 121,
    ];

    #[test]
    fn pkce_matches_rfc_7636() {
        let p = Pkce::from_random(&RFC_RANDOM);
        assert_eq!(p.verifier, "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
        assert_eq!(p.challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn authorize_url_has_every_parameter_encoded() {
        let url = authorize_url("abc123", "CHAL", "st8");
        assert!(url.starts_with("https://accounts.spotify.com/authorize?response_type=code&"));
        assert!(url.contains("client_id=abc123"));
        assert!(url.contains(
            "scope=user-read-currently-playing+user-read-playback-state+user-modify-playback-state"
        ));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A8888%2Fcallback"));
        assert!(url.contains("code_challenge_method=S256&code_challenge=CHAL&state=st8"));
    }

    #[test]
    fn callback_happy_path_with_whitespace() {
        let pasted = "  http://127.0.0.1:8888/callback?code=AQB%2Dx_y&state=st8\n";
        assert_eq!(parse_callback(pasted, "st8").as_deref(), Ok("AQB-x_y"));
    }

    #[test]
    fn callback_errors() {
        assert_eq!(
            parse_callback(
                "http://127.0.0.1:8888/callback?error=access_denied&state=st8",
                "st8"
            ),
            Err(CallbackError::Denied("access_denied".into()))
        );
        assert_eq!(
            parse_callback("http://127.0.0.1:8888/callback?code=abc&state=old", "st8"),
            Err(CallbackError::StateMismatch)
        );
        assert_eq!(
            parse_callback("http://127.0.0.1:8888/callback?code=abc", "st8"),
            Err(CallbackError::StateMismatch)
        );
        assert_eq!(
            parse_callback("http://127.0.0.1:8888/callback?state=st8", "st8"),
            Err(CallbackError::NotACallback)
        );
        assert_eq!(
            parse_callback("hello", "st8"),
            Err(CallbackError::NotACallback)
        );
        assert_eq!(parse_callback("", "st8"), Err(CallbackError::NotACallback));
    }

    #[test]
    fn bare_query_is_accepted() {
        assert_eq!(
            parse_callback("code=abc&state=st8", "st8").as_deref(),
            Ok("abc")
        );
    }

    #[test]
    fn request_bodies() {
        assert_eq!(
            code_exchange_body("cid", "c/de", "ver"),
            "grant_type=authorization_code&code=c%2Fde&redirect_uri=http%3A%2F%2F127.0.0.1%3A8888%2Fcallback&client_id=cid&code_verifier=ver"
        );
        assert_eq!(
            refresh_body("cid", "r+t"),
            "grant_type=refresh_token&refresh_token=r%2Bt&client_id=cid"
        );
    }

    #[test]
    fn token_responses() {
        let ok = parse_token_response(
            200,
            include_bytes!("../tests/fixtures/spotify_token_ok.json"),
        );
        assert_eq!(
            ok,
            Ok(Tokens {
                access_token: "BQD-access-token".into(),
                expires_in_s: 3600,
                refresh_token: Some("AQC-new-refresh-token".into()),
            })
        );
        let ok = parse_token_response(
            200,
            include_bytes!("../tests/fixtures/spotify_token_ok_no_refresh.json"),
        )
        .unwrap();
        assert_eq!(ok.refresh_token, None);

        let err = parse_token_response(
            400,
            include_bytes!("../tests/fixtures/spotify_token_invalid_grant.json"),
        );
        assert_eq!(err, Err(TokenError::InvalidGrant));
        assert_eq!(
            parse_token_response(400, br#"{"error":"invalid_client"}"#),
            Err(TokenError::InvalidClient)
        );
        assert_eq!(
            parse_token_response(400, br#"{"error":"unsupported_grant_type"}"#),
            Err(TokenError::Other("unsupported_grant_type".into()))
        );
        assert_eq!(
            parse_token_response(503, b"<html>down</html>"),
            Err(TokenError::Malformed { status: 503 })
        );
    }

    #[test]
    fn refresh_timing() {
        assert!(!needs_refresh(1_000, 1_000, 3600));
        assert!(!needs_refresh(1_000 + 3299, 1_000, 3600));
        assert!(needs_refresh(1_000 + 3300, 1_000, 3600));
        // A very short lifetime refreshes immediately rather than underflowing.
        assert!(needs_refresh(5, 5, 60));
    }
}
