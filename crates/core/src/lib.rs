//! Hardware-independent On-Air logic.
//!
//! Everything here builds for both the host (for tests and the simulator) and
//! the ESP32-S3. Keep it free of hardware and network code: callers pass in
//! time, random bytes and HTTP responses, and get back decisions and data.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod app;
pub mod auth;
pub mod backoff;
pub mod http;
pub mod image;
pub mod input;
pub mod portal;
pub mod schedule;
pub mod settings;
pub mod setup;
pub mod spotify;
pub mod spotify_sync;
pub mod timezone;
pub mod weather;

/// Encode `key=value&...` for query strings and form bodies.
pub(crate) fn form_encode(pairs: &[(&str, &str)]) -> alloc::string::String {
    let mut serializer = form_urlencoded::Serializer::new(alloc::string::String::new());
    for (key, value) in pairs {
        serializer.append_pair(key, value);
    }
    serializer.finish()
}

/// A response body that couldn't be parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseError;

impl core::fmt::Display for ParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("malformed response")
    }
}
