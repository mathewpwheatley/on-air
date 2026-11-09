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
    use rstest::rstest;

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

    #[rstest]
    #[case::start("https://accounts.spotify.com/authorize?response_type=code&")]
    #[case::client_id("client_id=abc123")]
    #[case::scopes(
        "scope=user-read-currently-playing+user-read-playback-state+user-modify-playback-state"
    )]
    #[case::redirect("redirect_uri=http%3A%2F%2F127.0.0.1%3A8888%2Fcallback")]
    #[case::challenge_and_state("code_challenge_method=S256&code_challenge=CHAL&state=st8")]
    fn authorize_url_has_every_parameter_encoded(#[case] part: &str) {
        let url = authorize_url("abc123", "CHAL", "st8");
        assert!(url.contains(part), "{url}");
    }

    #[rstest]
    #[case::pasted_with_whitespace(
        "  http://127.0.0.1:8888/callback?code=AQB%2Dx_y&state=st8\n",
        "AQB-x_y"
    )]
    #[case::bare_query("code=abc&state=st8", "abc")]
    fn callback_gives_the_code(#[case] pasted: &str, #[case] code: &str) {
        assert_eq!(parse_callback(pasted, "st8").as_deref(), Ok(code));
    }

    #[rstest]
    #[case::denied(
        "http://127.0.0.1:8888/callback?error=access_denied&state=st8",
        CallbackError::Denied("access_denied".into())
    )]
    #[case::old_state(
        "http://127.0.0.1:8888/callback?code=abc&state=old",
        CallbackError::StateMismatch
    )]
    #[case::no_state(
        "http://127.0.0.1:8888/callback?code=abc",
        CallbackError::StateMismatch
    )]
    #[case::no_code(
        "http://127.0.0.1:8888/callback?state=st8",
        CallbackError::NotACallback
    )]
    #[case::not_a_url("hello", CallbackError::NotACallback)]
    #[case::empty("", CallbackError::NotACallback)]
    fn callback_errors(#[case] pasted: &str, #[case] error: CallbackError) {
        assert_eq!(parse_callback(pasted, "st8"), Err(error));
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

    #[rstest]
    #[case::rotated_refresh_token(
        include_bytes!("../tests/fixtures/spotify_token_ok.json"),
        "BQD-access-token",
        Some("AQC-new-refresh-token"),
    )]
    #[case::same_refresh_token(
        include_bytes!("../tests/fixtures/spotify_token_ok_no_refresh.json"),
        "BQD-access-token-2",
        None,
    )]
    fn token_replies(
        #[case] body: &[u8],
        #[case] access_token: &str,
        #[case] refresh_token: Option<&str>,
    ) {
        assert_eq!(
            parse_token_response(200, body),
            Ok(Tokens {
                access_token: access_token.into(),
                expires_in_s: 3600,
                refresh_token: refresh_token.map(Into::into),
            })
        );
    }

    #[rstest]
    #[case::invalid_grant(
        400,
        include_bytes!("../tests/fixtures/spotify_token_invalid_grant.json"),
        TokenError::InvalidGrant,
    )]
    #[case::invalid_client(400, br#"{"error":"invalid_client"}"#, TokenError::InvalidClient)]
    #[case::other(
        400,
        br#"{"error":"unsupported_grant_type"}"#,
        TokenError::Other("unsupported_grant_type".into()),
    )]
    #[case::html_outage(503, b"<html>down</html>", TokenError::Malformed { status: 503 })]
    #[case::ok_but_unreadable(200, b"{}", TokenError::Malformed { status: 200 })]
    fn token_errors(#[case] status: u16, #[case] body: &[u8], #[case] error: TokenError) {
        assert_eq!(parse_token_response(status, body), Err(error));
    }

    #[rstest]
    #[case::just_issued(1_000, 1_000, 3600, false)]
    #[case::before_the_margin(1_000 + 3299, 1_000, 3600, false)]
    #[case::at_the_margin(1_000 + 3300, 1_000, 3600, true)]
    #[case::short_lifetime_refreshes_at_once(5, 5, 60, true)]
    fn refresh_timing(
        #[case] now_s: u64,
        #[case] issued_s: u64,
        #[case] lifetime_s: u32,
        #[case] needed: bool,
    ) {
        assert_eq!(needs_refresh(now_s, issued_s, lifetime_s), needed);
    }
}
