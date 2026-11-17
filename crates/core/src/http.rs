//! Plain HTTP requests and replies. Core logic hands these out instead of
//! doing network I/O itself, so the firmware and the simulator can each send
//! them with their own HTTP client.
//!
//! How a host sends a [`Request`]:
//! - `bearer`: add `Authorization: Bearer <token>`.
//! - `form_body`: send it with `Content-Type: application/x-www-form-urlencoded`.
//! - A `PUT` or `POST` with no body: send `Content-Length: 0`.
//! - Read the `Retry-After` header into [`Response::retry_after`].
//!
//! Hosts without an HTTP client (the firmware) can use [`target`],
//! [`encode`] and [`decode`] to speak HTTP/1.1 over any byte stream:
//!
//! ```text
//! let target = http::target(&request.url)?;      // host and path
//! connect_tls(target.host, 443);
//! stream.write_all(&http::encode(&request, &target));
//! loop {
//!     received.extend(stream.read());
//!     match http::decode(&received, closed) { Complete(response) => break, Incomplete => continue, Invalid => fail }
//! }
//! ```

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Put,
    Post,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: Method,
    pub url: String,
    pub bearer: Option<String>,
    pub form_body: Option<String>,
}

impl Request {
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            method: Method::Get,
            url: url.into(),
            bearer: None,
            form_body: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    /// The `Retry-After` header, if any.
    pub retry_after: Option<String>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            retry_after: None,
            body: body.into(),
        }
    }
}

/// The request got no reply at all: no network, DNS failure, timeout or a
/// TLS error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkError;

/// What sending a request produced.
pub type Outcome = Result<Response, NetworkError>;

/// Where a request goes, from its `https://` URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target<'a> {
    pub host: &'a str,
    /// The path and query, starting with `/`.
    pub path: &'a str,
}

/// Split an `https://` URL into host and path. `None` for anything else,
/// since every host the device talks to uses HTTPS on port 443.
pub fn target(url: &str) -> Option<Target<'_>> {
    let rest = url.strip_prefix("https://")?;
    let (host, path) = match rest.find(['/', '?']) {
        Some(i) if rest.as_bytes()[i] == b'/' => (&rest[..i], &rest[i..]),
        // A query straight after the host: none of our hosts need one.
        Some(_) => return None,
        None => (rest, "/"),
    };
    (!host.is_empty() && !host.contains([':', '@'])).then_some(Target { host, path })
}

/// The bytes to send for `request`: HTTP/1.1, asking the server to close the
/// connection after replying.
pub fn encode(request: &Request, target: &Target) -> Vec<u8> {
    let method = match request.method {
        Method::Get => "GET",
        Method::Put => "PUT",
        Method::Post => "POST",
    };
    let mut head = format!(
        "{method} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: On-Air\r\nAccept: */*\r\nConnection: close\r\n",
        target.path, target.host
    );
    if let Some(token) = &request.bearer {
        head.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    let body = request.form_body.as_deref().unwrap_or("");
    if request.form_body.is_some() {
        head.push_str("Content-Type: application/x-www-form-urlencoded\r\n");
    }
    if request.form_body.is_some() || request.method != Method::Get {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");
    head.push_str(body);
    head.into_bytes()
}

/// How far [`decode`] got with the bytes received so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decoded {
    /// Read more, then try again.
    Incomplete,
    Complete(Response),
    /// Not a valid HTTP response, or the connection closed part-way.
    Invalid,
}

/// Decode a response from everything received so far. `closed` is whether
/// the server has closed the connection, which ends a body that has
/// neither a length nor chunks.
pub fn decode(received: &[u8], closed: bool) -> Decoded {
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut parsed = httparse::Response::new(&mut headers);
    let head_len = match parsed.parse(received) {
        Ok(httparse::Status::Complete(len)) => len,
        Ok(httparse::Status::Partial) if !closed => return Decoded::Incomplete,
        _ => return Decoded::Invalid,
    };
    let Some(status) = parsed.code else {
        return Decoded::Invalid;
    };
    let header = |name: &str| {
        parsed
            .headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case(name))
            .and_then(|h| core::str::from_utf8(h.value).ok())
            .map(str::trim)
    };
    let retry_after = header("retry-after").map(String::from);
    let chunked = header("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked"));
    let content_length = header("content-length").map(|v| v.parse::<usize>());
    let body = &received[head_len..];
    let complete = |body: Vec<u8>| {
        Decoded::Complete(Response {
            status,
            retry_after: retry_after.clone(),
            body,
        })
    };

    // These never have a body.
    if status == 204 || status == 304 || (100..200).contains(&status) {
        return complete(Vec::new());
    }
    if chunked {
        return match dechunk(body) {
            Some(Some(body)) => complete(body),
            Some(None) if !closed => Decoded::Incomplete,
            _ => Decoded::Invalid,
        };
    }
    match content_length {
        Some(Ok(length)) if body.len() >= length => complete(body[..length].to_vec()),
        Some(Ok(_)) if !closed => Decoded::Incomplete,
        Some(_) => Decoded::Invalid,
        None if closed => complete(body.to_vec()),
        None => Decoded::Incomplete,
    }
}

/// Join a chunked body. `Some(None)` while chunks are still to come, `None`
/// if the chunk framing is broken.
fn dechunk(mut received: &[u8]) -> Option<Option<Vec<u8>>> {
    let mut body = Vec::new();
    loop {
        let (size_len, size) = match httparse::parse_chunk_size(received) {
            Ok(httparse::Status::Complete(parsed)) => parsed,
            Ok(httparse::Status::Partial) => return Some(None),
            Err(_) => return None,
        };
        let size = usize::try_from(size).ok()?;
        let rest = &received[size_len..];
        if size == 0 {
            // The last chunk; ignore any trailers once the blank line is in.
            return Some(rest.windows(2).any(|w| w == b"\r\n").then_some(body));
        }
        if rest.len() < size + 2 {
            return Some(None);
        }
        if &rest[size..size + 2] != b"\r\n" {
            return None;
        }
        body.extend_from_slice(&rest[..size]);
        received = &rest[size + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case::path_and_query(
        "https://api.spotify.com/v1/me/player?market=from_token",
        Some(("api.spotify.com", "/v1/me/player?market=from_token")),
    )]
    #[case::bare_host("https://i.scdn.co", Some(("i.scdn.co", "/")))]
    #[case::plain_http("http://example.com/", None)]
    #[case::port("https://example.com:8443/", None)]
    #[case::no_host("https:///path", None)]
    fn targets(#[case] url: &str, #[case] expected: Option<(&str, &str)>) {
        assert_eq!(
            target(url),
            expected.map(|(host, path)| Target { host, path })
        );
    }

    fn text(bytes: Vec<u8>) -> String {
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn encodes_a_get_with_a_bearer_token() {
        let request = Request {
            bearer: Some("abc".into()),
            ..Request::get("https://api.spotify.com/v1/me/player")
        };
        let target = target(&request.url).unwrap();
        assert_eq!(
            text(encode(&request, &target)),
            "GET /v1/me/player HTTP/1.1\r\nHost: api.spotify.com\r\nUser-Agent: On-Air\r\n\
             Accept: */*\r\nConnection: close\r\nAuthorization: Bearer abc\r\n\r\n"
        );
    }

    #[test]
    fn encodes_a_form_post() {
        let request = Request {
            method: Method::Post,
            url: "https://accounts.spotify.com/api/token".into(),
            bearer: None,
            form_body: Some("grant_type=refresh_token".into()),
        };
        let encoded = text(encode(&request, &target(&request.url).unwrap()));
        assert!(encoded.starts_with("POST /api/token HTTP/1.1\r\n"));
        assert!(encoded.contains("Content-Type: application/x-www-form-urlencoded\r\n"));
        assert!(encoded.ends_with("Content-Length: 24\r\n\r\ngrant_type=refresh_token"));
    }

    #[test]
    fn encodes_an_empty_put_with_zero_length() {
        let request = Request {
            method: Method::Put,
            ..Request::get("https://api.spotify.com/v1/me/player/play")
        };
        let encoded = text(encode(&request, &target(&request.url).unwrap()));
        assert!(encoded.ends_with("Content-Length: 0\r\n\r\n"));
    }

    const OK_HEAD: &str = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n";

    #[rstest]
    #[case::length_complete("Content-Length: 5\r\n\r\nhello", false, Some("hello"))]
    #[case::length_with_extra("Content-Length: 5\r\n\r\nhello!!", false, Some("hello"))]
    #[case::length_partial("Content-Length: 5\r\n\r\nhel", false, None)]
    #[case::chunked_complete(
        "Transfer-Encoding: chunked\r\n\r\n3\r\nhel\r\n2\r\nlo\r\n0\r\n\r\n",
        false,
        Some("hello")
    )]
    #[case::chunked_partial("Transfer-Encoding: chunked\r\n\r\n3\r\nhel\r\n2\r\nl", false, None)]
    #[case::no_length_until_closed("\r\nhello", false, None)]
    #[case::no_length_closed("\r\nhello", true, Some("hello"))]
    fn decodes_bodies(#[case] rest: &str, #[case] closed: bool, #[case] body: Option<&str>) {
        let received = format!("{OK_HEAD}{rest}");
        let expected = match body {
            Some(body) => Decoded::Complete(Response::new(200, body)),
            None => Decoded::Incomplete,
        };
        assert_eq!(decode(received.as_bytes(), closed), expected);
    }

    #[test]
    fn decodes_retry_after_and_an_empty_204() {
        let received =
            b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 12\r\nContent-Length: 0\r\n\r\n";
        let Decoded::Complete(response) = decode(received, false) else {
            panic!("expected a response");
        };
        assert_eq!(
            (response.status, response.retry_after.as_deref()),
            (429, Some("12"))
        );
        assert_eq!(
            decode(b"HTTP/1.1 204 No Content\r\n\r\n", false),
            Decoded::Complete(Response::new(204, Vec::new()))
        );
    }

    #[rstest]
    #[case::partial_head(b"HTTP/1.1 200 OK\r\nContent-", false, Decoded::Incomplete)]
    #[case::closed_mid_head(b"HTTP/1.1 200 OK\r\nContent-", true, Decoded::Invalid)]
    #[case::closed_mid_body(
        b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nshort",
        true,
        Decoded::Invalid
    )]
    #[case::not_http(b"hello there\r\n\r\n", false, Decoded::Invalid)]
    #[case::bad_chunk(
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n",
        false,
        Decoded::Invalid
    )]
    fn incomplete_and_invalid(
        #[case] received: &[u8],
        #[case] closed: bool,
        #[case] expected: Decoded,
    ) {
        assert_eq!(decode(received, closed), expected);
    }
}
