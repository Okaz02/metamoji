//! The HTTP seam the TypeScript client runs on.
//!
//! `metamoji-api` is a TypeScript client for the same API this app speaks, and
//! moving a call up to it needs two things a webview cannot do on its own.
//!
//! * **Origin.** The server sends no CORS headers, and a page served from
//!   `tauri://` may not read a response from `mps.metamoji.com`. Requests made
//!   here have no origin to answer for.
//! * **The session.** The session *is* a cookie (docs/typespec/README.md §認証),
//!   and it is one session, not one per HTTP stack. Rust still makes the calls
//!   the webview cannot — the drive service's binary payloads, the relay's own
//!   REST — so a second jar in the webview would be a second, diverging login.
//!
//! So the webview's client is given a transport that comes back down here and
//! goes out through `CloudClient`'s own `reqwest` client. One jar, one
//! session file, one set of `X-DM-*` headers, whichever side made the call.

use std::collections::HashMap;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::cloud::{CloudClient, NOT_LOGIN_EXCEPTION};
use crate::error::{AppError, AppResult};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchRequest {
    pub url: String,
    pub method: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Base64, because a request body may be a zip and JSON has no bytes.
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchResponse {
    pub status: u16,
    pub status_text: String,
    pub headers: HashMap<String, String>,
    /// Kept apart from `headers`: a map cannot hold two of them, and a login
    /// can set several at once.
    pub set_cookie: Vec<String>,
    /// Base64, for the same reason as the request body.
    pub body: String,
}

/// Runs a request, signing in again once if the session had lapsed.
///
/// The client above cannot do this for itself: it has no credential, and it
/// is not going to be given one. Recovering here means a call made from the
/// webview survives an expired session exactly as one made in Rust does —
/// which is the difference between a lesson carrying on and a screen full of
/// "It doesn't log it in."
pub async fn fetch(cloud: &CloudClient, request: FetchRequest) -> AppResult<FetchResponse> {
    let first = send(cloud, &request).await?;
    if !says_not_logged_in(&first) {
        return Ok(first);
    }
    if cloud.refresh_session().await.is_err() {
        return Ok(first);
    }
    send(cloud, &request).await
}

/// Whether the server's answer is "your session has lapsed".
///
/// Read from the body, never the status. A signed-out `users2/login/user`
/// answers **HTTP 500** with `NotLoginException` — the status says only that
/// something went wrong, and treating every 500 as a lapsed session would sign
/// the user in again over any server fault.
///
/// Both envelope shapes, because the client above reaches both families: the
/// `CsCloudService` one carries `errorCode` at the top level, the newer
/// `SdCloudService` one nests it under `data`.
fn says_not_logged_in(response: &FetchResponse) -> bool {
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&response.body) else {
        return false;
    };
    let Ok(body) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    let code = body
        .get("data")
        .and_then(|data| data.get("errorCode"))
        .or_else(|| body.get("errorCode"))
        .and_then(serde_json::Value::as_i64);
    code == Some(NOT_LOGIN_EXCEPTION)
}

async fn send(cloud: &CloudClient, request: &FetchRequest) -> AppResult<FetchResponse> {
    let method = reqwest::Method::from_bytes(request.method.as_bytes())
        .map_err(|_| AppError::other(format!("不明なメソッド: {}", request.method)))?;

    let mut outgoing = cloud.http().request(method, &request.url);
    for (name, value) in &request.headers {
        outgoing = outgoing.header(name, value);
    }
    if let Some(body) = &request.body {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(body)
            .map_err(|e| AppError::other(format!("本文を復元できません: {e}")))?;
        outgoing = outgoing.body(bytes);
    }

    let response = outgoing
        .send()
        .await
        .map_err(|e| AppError::other(format!("接続できません: {e}")))?;

    let status = response.status();
    let mut headers = HashMap::new();
    let mut set_cookie = Vec::new();
    for (name, value) in response.headers() {
        let Ok(value) = value.to_str() else { continue };
        if name.as_str().eq_ignore_ascii_case("set-cookie") {
            set_cookie.push(value.to_string());
        } else {
            headers.insert(name.as_str().to_ascii_lowercase(), value.to_string());
        }
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| AppError::other(format!("応答を読み取れません: {e}")))?;

    // The jar took the cookies on the way past; this is what makes the two
    // sides one session. Written out here so a sign-in made from the webview
    // survives a restart, exactly as one made down here does.
    cloud.persist_cookies();

    Ok(FetchResponse {
        status: status.as_u16(),
        status_text: status.canonical_reason().unwrap_or_default().to_string(),
        headers,
        set_cookie,
        body: base64::engine::general_purpose::STANDARD.encode(&bytes),
    })
}

#[cfg(test)]
#[path = "transport/tests.rs"]
mod tests;
