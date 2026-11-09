//! Plain HTTP requests and replies. Core logic hands these out instead of
//! doing network I/O itself, so the firmware and the simulator can each send
//! them with their own HTTP client.
//!
//! How a host sends a [`Request`]:
//! - `bearer`: add `Authorization: Bearer <token>`.
//! - `form_body`: send it with `Content-Type: application/x-www-form-urlencoded`.
//! - A `PUT` or `POST` with no body: send `Content-Length: 0`.
//! - Read the `Retry-After` header into [`Response::retry_after`].

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
