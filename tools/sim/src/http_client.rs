//! Sends the core's HTTP requests with `reqwest`. The firmware does the same
//! with `reqwless`.

use std::time::Duration;

use on_air_core::http::{Method, NetworkError, Outcome, Request, Response};
use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, RETRY_AFTER};

pub fn client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("HTTP client")
}

pub fn send(client: &Client, request: &Request) -> Outcome {
    let mut builder = match request.method {
        Method::Get => client.get(&request.url),
        Method::Put => client.put(&request.url),
        Method::Post => client.post(&request.url),
    };
    if let Some(token) = &request.bearer {
        builder = builder.header(AUTHORIZATION, format!("Bearer {token}"));
    }
    match &request.form_body {
        Some(body) => {
            builder = builder
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(body.clone());
        }
        None if request.method != Method::Get => builder = builder.header(CONTENT_LENGTH, "0"),
        None => {}
    }
    let response = builder.send().map_err(|_| NetworkError)?;
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let body = response.bytes().map_err(|_| NetworkError)?.to_vec();
    Ok(Response {
        status,
        retry_after,
        body,
    })
}
