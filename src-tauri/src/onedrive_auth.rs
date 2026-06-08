//! Interactive OneDrive OAuth: auth-code + PKCE in the system browser, with the
//! redirect delivered via a **custom URI scheme** (`wonderblob://auth`) caught by
//! `tauri-plugin-deep-link` — NOT a localhost loopback listener.
//!
//! Why a custom scheme (vs the plan's loopback): Jack registered the redirect
//! URI `wonderblob://auth` in Entra, so the OS protocol handler (a `.desktop`
//! handler on Linux, registered by the deep-link plugin) receives the callback
//! deep link `wonderblob://auth?code=...&state=...`.
//!
//! The pure HTTP token calls live in core
//! (`wonderblob_core::onedrive::{exchange_code, refresh_tokens}`); this module
//! owns only the non-headless browser-open + deep-link-await half, which can't be
//! unit-tested headless and is exercised by the manual OAuth smoke (Task 12/13).

use base64::Engine as _;
use sha2::{Digest, Sha256};
use std::time::Duration;
use tauri_plugin_deep_link::DeepLinkExt;
use wonderblob_core::error::StorageError;

/// SHIPPED multi-tenant public client ID (Jack's Entra app registration).
/// This is NOT a secret — public/native clients send no client_secret.
pub const DEFAULT_CLIENT_ID: &str = "aaeb21a2-1c76-4c1d-92ab-28c6e611dcc2";

/// Work/school accounts only (OneDrive for Business). `common` would also allow
/// personal accounts, which are explicitly deferred.
pub const AUTH_BASE: &str = "https://login.microsoftonline.com/organizations/oauth2/v2.0";

/// The custom-scheme redirect registered in Entra under "Mobile and desktop
/// applications". Caught by the deep-link plugin.
pub const REDIRECT_URI: &str = "wonderblob://auth";

/// The URI scheme half of `REDIRECT_URI`, used for runtime registration in dev.
pub const SCHEME: &str = "wonderblob";

/// Overall cap so an abandoned browser sign-in can't hang forever.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// (verifier, S256 challenge). The verifier is 64 URL-safe chars; the challenge
/// is base64url(SHA256(verifier)) with no padding.
fn pkce() -> (String, String) {
    let verifier = random_urlsafe(64);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

/// A URL-safe random string (PKCE verifier / CSRF state). Uses the OS RNG via
/// `uuid`'s random bytes (already a dep) folded into base64url, avoiding a new
/// `rand` dependency.
fn random_urlsafe(len: usize) -> String {
    let mut bytes = Vec::with_capacity(len);
    while bytes.len() < len {
        bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    }
    bytes.truncate(len);
    let s = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&bytes);
    s.chars().take(len).collect()
}

pub struct LoginResult {
    pub refresh_token: String,
    pub account_label: Option<String>,
}

/// Build the authorize URL (split out so it can be reasoned about / future
/// unit-tested). `redirect_uri` and `scope` are percent-encoded.
fn authorize_url(client_id: &str, redirect_uri: &str, challenge: &str, state: &str) -> String {
    let enc = |s: &str| {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect::<String>()
    };
    format!(
        "{AUTH_BASE}/authorize?client_id={client_id}&response_type=code\
         &redirect_uri={}&response_mode=query&scope={}\
         &code_challenge={challenge}&code_challenge_method=S256&state={state}",
        enc(redirect_uri),
        enc(wonderblob_core::onedrive::SCOPES),
    )
}

/// Run the full interactive flow: generate PKCE+state, open the system browser to
/// the authorize URL with `redirect_uri=wonderblob://auth`, await the deep-link
/// callback carrying `code`+`state`, validate state, and exchange code+verifier
/// for tokens at the token endpoint. `client_id` defaults to `DEFAULT_CLIENT_ID`.
pub async fn interactive_login(
    app: &tauri::AppHandle,
    client_id: &str,
) -> Result<LoginResult, StorageError> {
    use tokio::sync::oneshot;

    let (verifier, challenge) = pkce();
    let state = random_urlsafe(32);
    let url = authorize_url(client_id, REDIRECT_URI, &challenge, &state);

    // Subscribe to the deep-link callback BEFORE opening the browser so we can't
    // miss a fast redirect. The plugin delivers full `wonderblob://auth?...` URLs.
    let (tx, rx) = oneshot::channel::<String>();
    let tx = std::sync::Mutex::new(Some(tx));
    app.deep_link().on_open_url(move |event| {
        for u in event.urls() {
            let s = u.to_string();
            if s.starts_with(REDIRECT_URI) || s.starts_with(&format!("{SCHEME}://")) {
                if let Some(tx) = tx.lock().unwrap().take() {
                    let _ = tx.send(s);
                }
                break;
            }
        }
    });

    // Open the system browser. (tauri-plugin-opener is already a dep.)
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(StorageError::other)?;

    // Await the callback (with an overall timeout).
    let callback = tokio::time::timeout(LOGIN_TIMEOUT, rx)
        .await
        .map_err(|_| StorageError::Network {
            detail: "sign-in timed out waiting for the browser redirect".into(),
        })?
        .map_err(|_| StorageError::AuthFailed {
            detail: "sign-in was cancelled".into(),
        })?;

    let (code, got_state) = parse_callback(&callback)?;
    if got_state != state {
        return Err(StorageError::AuthFailed {
            detail: "OAuth state mismatch (possible CSRF)".into(),
        });
    }

    let client = reqwest::Client::new();
    let tr = wonderblob_core::onedrive::exchange_code(
        &client,
        AUTH_BASE,
        client_id,
        &code,
        &verifier,
        REDIRECT_URI,
    )
    .await?;
    let refresh_token = tr.refresh_token.ok_or(StorageError::AuthFailed {
        detail: "token response had no refresh token (offline_access scope?)".into(),
    })?;
    let account_label = tr.id_token.as_deref().and_then(account_label_from_id_token);
    Ok(LoginResult {
        refresh_token,
        account_label,
    })
}

/// Register the custom scheme at runtime. On Linux the `.desktop` handler is only
/// installed for packaged builds, so `cargo tauri dev` needs `register_all()`.
/// Best-effort: a failure here only means deep links won't be caught in dev.
pub fn register_scheme(app: &tauri::AppHandle) {
    #[cfg(any(target_os = "linux", all(debug_assertions, windows)))]
    {
        let _ = app.deep_link().register_all();
    }
    #[cfg(not(any(target_os = "linux", all(debug_assertions, windows))))]
    {
        let _ = app;
    }
}

/// Extract `(code, state)` from a `wonderblob://auth?code=...&state=...` URL.
fn parse_callback(url: &str) -> Result<(String, String), StorageError> {
    let query = url.split_once('?').map(|(_, q)| q).unwrap_or("");
    let mut code = None;
    let mut state = None;
    let mut error = None;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = percent_decode(v);
        match k {
            "code" => code = Some(v),
            "state" => state = Some(v),
            "error" => error = Some(v),
            "error_description" if error.is_some() => {
                error = Some(format!("{}: {}", error.take().unwrap_or_default(), v));
            }
            _ => {}
        }
    }
    if let Some(e) = error {
        return Err(StorageError::AuthFailed {
            detail: format!("authorization error: {e}"),
        });
    }
    match (code, state) {
        (Some(c), Some(s)) => Ok((c, s)),
        _ => Err(StorageError::AuthFailed {
            detail: "redirect missing code/state".into(),
        }),
    }
}

/// Minimal `application/x-www-form-urlencoded` percent-decoder (also handles `+`).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                if let Ok(b) = u8::from_str_radix(hex, 16) {
                    out.push(b);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Decode the `preferred_username`/`name`/`upn` claim from the (unverified) JWT
/// id_token payload for a display label. No signature verification — it's display
/// metadata only, never an authorization decision.
fn account_label_from_id_token(id_token: &str) -> Option<String> {
    let payload_b64 = id_token.split('.').nth(1)?;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&payload).ok()?;
    json.get("preferred_username")
        .or_else(|| json.get("upn"))
        .or_else(|| json.get("email"))
        .or_else(|| json.get("name"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_is_s256_of_verifier() {
        let (verifier, challenge) = pkce();
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(challenge, expected);
        // base64url(SHA256) with no padding is always 43 chars.
        assert_eq!(challenge.len(), 43);
        assert!(!challenge.contains('='));
        assert!(!challenge.contains('+'));
        assert!(!challenge.contains('/'));
    }

    #[test]
    fn authorize_url_has_required_params() {
        let url = authorize_url("CID", REDIRECT_URI, "CHAL", "STATE");
        assert!(url.starts_with(&format!("{AUTH_BASE}/authorize?")));
        assert!(url.contains("client_id=CID"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("code_challenge=CHAL"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=STATE"));
        // Custom-scheme redirect, percent-encoded.
        assert!(url.contains("redirect_uri=wonderblob%3A%2F%2Fauth"));
        // Scopes percent-encoded (spaces -> %20).
        assert!(url.contains("Files.ReadWrite.All%20offline_access"));
    }

    #[test]
    fn parse_callback_extracts_code_and_state() {
        let (code, state) =
            parse_callback("wonderblob://auth?code=abc123&state=xyz&session_state=foo").unwrap();
        assert_eq!(code, "abc123");
        assert_eq!(state, "xyz");
    }

    #[test]
    fn parse_callback_surfaces_error() {
        let err = parse_callback("wonderblob://auth?error=access_denied&error_description=nope")
            .unwrap_err();
        assert!(matches!(err, StorageError::AuthFailed { .. }));
    }

    #[test]
    fn account_label_decodes_preferred_username() {
        // {"preferred_username":"jack@contoso.com"} as a JWT middle segment.
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"preferred_username":"jack@contoso.com","name":"Jack"}"#);
        let jwt = format!("h.{payload}.sig");
        assert_eq!(
            account_label_from_id_token(&jwt).as_deref(),
            Some("jack@contoso.com")
        );
    }

    #[test]
    fn random_urlsafe_has_requested_len() {
        assert_eq!(random_urlsafe(64).len(), 64);
        assert_eq!(random_urlsafe(32).len(), 32);
        assert_ne!(random_urlsafe(32), random_urlsafe(32));
    }
}
