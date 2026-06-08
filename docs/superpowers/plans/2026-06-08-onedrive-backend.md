# Wonderblob Plan 5: OneDrive for Business Backend (Microsoft Graph + OAuth 2.0 PKCE)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a fourth `StorageBackend` — OneDrive for Business — driven over the Microsoft Graph REST API with `reqwest`, authenticated via the OAuth 2.0 authorization-code + PKCE flow in the user's **system browser** (public client, no secret), wired end-to-end through bookmarks, the Tauri command layer, and the 1Password-8-style frontend, including a native "Share Link" action backed by Graph `createLink`. The backend passes a dedicated **mocked-Graph** test suite in CI (Graph is not self-hostable like MinIO/Azurite), plus a manual interactive-OAuth smoke checklist and an env-gated real-tenant test.

**Architecture:** A new UI-agnostic module `crates/wonderblob-core/src/onedrive.rs` implements `StorageBackend` unchanged. Unlike S3/Azure (flat key namespace synthesized into a tree via `objstore.rs`), **OneDrive has real folders**, so listing/stat use Graph's **path-addressed** DriveItem URLs (`/me/drive/root:/path/to/item:` and `…:/children`) directly — `objstore.rs`'s `ObjPath`/`PART_SIZE` are **not reused**; only the tiny `basename` helper is shared. The core backend takes an **injected token provider** (`Arc<dyn TokenProvider>`) and a configurable `base_url`, so the entire backend is testable against a `wiremock` mock Graph server with zero real auth. The non-headless parts — generating PKCE, opening the system browser (`tauri-plugin-opener`, already a dep), and catching the redirect on a one-shot **loopback HTTP listener** (hand-rolled `tokio::net::TcpListener`, zero new deps) — live in `src-tauri/src/onedrive_auth.rs`. Token-endpoint calls (code→token exchange, refresh) live in core as pure async fns so they're mock-tested too. Refresh tokens persist in the OS keychain (reusing `bookmarks::secrets`) keyed by the bookmark UUID; access tokens stay in-memory with expiry-based silent refresh.

**Tech Stack:** Rust (`reqwest` with `json`+`stream`, `tokio`, `async-trait`, `bytes`, `serde`, `base64`, `sha2`, `tempfile` for upload spill), Tauri 2.x (`tauri-plugin-opener`), Svelte 5 + Vite, `wiremock` (dev-dep, mocked Graph in CI).

**Spec:** `docs/superpowers/specs/2026-06-07-wonderblob-design.md` (OneDrive auth + backend rows; "Auth & credentials" → OneDrive bullet)
**Builds on:** Plans 1–4 (merged): `2026-06-07-foundation-sftp-slice.md`, `2026-06-08-s3-azure-backends.md`, `2026-06-08-transfer-engine.md`, `2026-06-08-edit-session.md`.

---

## ⚠️ HUMAN SETUP (Jack) — Entra app registration (blocks real-tenant testing only)

The app ships a **default multi-tenant public client ID** baked in as a constant, with a per-connection override (spec). That client ID comes from an app registration **only Jack can create** in the Entra admin center. **Every code task below proceeds with a clearly-marked `PLACEHOLDER_CLIENT_ID` constant and is fully testable against the `wiremock` mock server** — no real tenant needed. Only the *interactive OAuth smoke* (Task 12) and the *env-gated real-tenant test* (Task 13) are blocked until Jack supplies the ID.

**Exact steps to ask Jack for (Microsoft Entra admin center → https://entra.microsoft.com):**

1. **Identity → Applications → App registrations → New registration.**
2. **Name:** `Wonderblob` (user-facing on the consent screen).
3. **Supported account types:** *Accounts in any organizational directory (Any Microsoft Entra ID tenant – Multitenant)*. (Work/school only; personal consumer OneDrive is explicitly deferred — see end.)
4. **Redirect URI:** leave blank here; set it under Authentication next.
5. **Register.** Copy the **Application (client) ID** (GUID) from the Overview page → this becomes `DEFAULT_CLIENT_ID`.
6. **Manage → Authentication → Add a platform → Mobile and desktop applications.**
7. Under **Custom redirect URIs**, add exactly: `http://localhost`
   - Grounding: for desktop apps using the **system browser**, Microsoft recommends `http://localhost`; the portal text box accepts `http://localhost` (the `http` scheme is allowed for loopback). Per RFC 8252 §7.3/8.3 the **port is ignored** when matching a `localhost` redirect, so our app can bind any ephemeral port at runtime. (`reply-url#supported-schemes`, `scenario-desktop-app-configuration`.)
   - Note: `http://127.0.0.1` is *preferred* over `localhost` by Microsoft, **but** the portal text box rejects an `http` loopback on the literal IP — it requires editing the app manifest `replyUrlsWithType`. To keep this a portal-only step, we register `http://localhost` and at runtime send `redirect_uri=http://localhost:<port>` (browser resolves `localhost`→loopback). The 127.0.0.1-via-manifest path is left as an optional hardening note.
8. Still under **Authentication → Advanced settings**, set **Allow public client flows = Yes** (this app is a public client; it sends no secret). (`scenario-desktop-app-configuration#enable-public-client-flow`.)
9. **Manage → API permissions → Add a permission → Microsoft Graph → Delegated permissions**, add:
   - `Files.ReadWrite.All` (browse/read/write the user's OneDrive)
   - `offline_access` (required to receive a **refresh token**)
   - `User.Read` (read `/me` for the account label)
   - (`openid` + `profile` are implied by OIDC and used to get an `id_token` for the account label.)
10. Leave admin consent as default; each user consents at first sign-in. Optionally **Grant admin consent** for Jack's own tenant to skip the per-user consent prompt.
11. Send the implementer the **Application (client) ID** → replace `PLACEHOLDER_CLIENT_ID` with it as `DEFAULT_CLIENT_ID`.

**Blocked-on-Jack:** Task 12 step "interactive smoke" and Task 13 (`WONDERBLOB_TEST_ONEDRIVE`). **Everything else proceeds now.**

---

## Graph / OAuth facts confirmed via Microsoft Learn (cite in code comments)

| Concern | Decision (grounded) | Source URL |
|---|---|---|
| Authority/endpoints | `https://login.microsoftonline.com/organizations/oauth2/v2.0/authorize` + `…/token` (`organizations` = work/school only) | `entra/identity-platform/v2-oauth2-auth-code-flow` |
| PKCE | `code_challenge` + `code_challenge_method=S256`; send matching `code_verifier` on token exchange; `state` for CSRF | same `#request-an-authorization-code` |
| Public client | native apps **must not** send `client_secret`/cert when redeeming the code | same `#redeem-a-code-for-an-access-token` |
| Redirect URI | `http://localhost` registered under "Mobile and desktop applications"; **port ignored** for loopback matching | `entra/identity-platform/reply-url#supported-schemes` |
| Refresh | `grant_type=refresh_token` + `refresh_token` + `client_id` + `scope`; no secret; response **may rotate** the refresh token | same `#refresh-the-access-token`, `graph/auth-v2-user` |
| Scopes | delegated `Files.ReadWrite.All offline_access User.Read` (+ `openid profile`) | `graph/permissions-reference` |
| List root / by path | `GET /me/drive/root/children`; `GET /me/drive/root:/{path}:/children` | `graph/api/resources/onedrive#commonly-accessed-resources`, `graph/api/driveitem-list-children` |
| Stat (+eTag) | `GET /me/drive/root:/{path}:` → DriveItem: `size`, `lastModifiedDateTime`, `file`/`folder` facet, `eTag`/`cTag`; `if-none-match`→304 | `graph/api/driveitem-get`, `graph/api/resources/driveitem` |
| Download (ranged) | `GET …:/content` 302→`@microsoft.graph.downloadUrl`; **apply `Range` to the downloadUrl**, not `/content` → `206 Partial Content` | `graph/api/driveitem-get-content#partial-range-downloads` |
| Small upload | `PUT /me/drive/root:/{path}:/content` (≤4 MiB recommended) | `graph/api/driveitem-put-content` |
| Large/resumable | `POST …:/createUploadSession` → `uploadUrl`; sequential `PUT` with `Content-Range: bytes a-b/total`; **fragments must be a multiple of 320 KiB (327,680 B)**, <60 MiB, last fragment any size; `404`→session gone (restart), `5xx`→retry w/ backoff | `graph/api/driveitem-createuploadsession#best-practices` |
| Delete | `DELETE /me/drive/root:/{path}:` | `graph/api/driveitem-delete` |
| Rename/move | `PATCH /me/drive/items/{id}` with `name` and/or `parentReference`; `if-match`→`412 Precondition Failed` on eTag mismatch | `graph/api/driveitem-move`, `graph/api/driveitem-update` |
| mkdir | `POST …:/children` `{ "name", "folder": {}, "@microsoft.graph.conflictBehavior": "fail" }` | `graph/api/driveitem-post-children` |
| Share link | `POST …:/createLink` `{ "type":"view", "scope":"organization" }` → `permission.link.webUrl`; `201` new / `200` existing; `expirationDateTime` supported. Org-scoped by default for OneDrive for Business; `anonymous` may be admin-disabled. | `graph/api/driveitem-createlink` |

**eTag conflict — a real improvement over S3/Azure:** S3/Azure rename/overwrite are last-writer-wins. Graph supports `if-match: {eTag}` on `PATCH`/upload-session commit, returning `412`. We map `412` → `StorageError::Conflict`, giving EditSession's save-back a true optimistic-concurrency guard. The backend exposes the captured `eTag` via `stat` (in `Entry`? No — `Entry` is fixed; the EditSession integration stays out of scope here and is noted as deferred wiring). For this plan, `if-match` is wired into `rename` (move) where we already hold the source item, and the conflict path is unit-tested.

---

### Task 1: Dependencies + Graph path helper

**Files:**
- Modify: `crates/wonderblob-core/Cargo.toml` (add `reqwest`, dev-dep `wiremock`)
- Modify: `src-tauri/Cargo.toml` (add `reqwest`)
- Create: `crates/wonderblob-core/src/onedrive.rs` (path helpers only this task)
- Modify: `crates/wonderblob-core/src/lib.rs` (`pub mod onedrive;`)

- [ ] **Step 1: Add deps.** In `crates/wonderblob-core/Cargo.toml`:
  ```toml
  reqwest = { version = "0.12", default-features = false, features = ["json", "stream", "rustls-tls"] }
  ```
  (`reqwest` is already a transitive dep via the AWS/Azure SDKs — see `Cargo.lock` — so this only promotes it to a direct dep. `rustls-tls` avoids a system OpenSSL requirement.) Under `[dev-dependencies]`:
  ```toml
  wiremock = "0.6"
  ```
  In `src-tauri/Cargo.toml` `[dependencies]`: `reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }`.

- [ ] **Step 2: Write the failing test** for path addressing. In `onedrive.rs`:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;
      #[test]
      fn item_and_children_urls_are_path_addressed() {
          let b = "https://graph.microsoft.com/v1.0";
          assert_eq!(children_url(b, "/"), format!("{b}/me/drive/root/children"));
          assert_eq!(item_url(b, "/"), format!("{b}/me/drive/root"));
          assert_eq!(
              children_url(b, "/Documents/Reports"),
              format!("{b}/me/drive/root:/Documents/Reports:/children")
          );
          assert_eq!(
              item_url(b, "/Documents/a b.txt"),
              format!("{b}/me/drive/root:/Documents/a%20b.txt:")
          );
      }
  }
  ```

- [ ] **Step 3: Run to verify failure** — `cargo test -p wonderblob-core onedrive` → FAIL (undefined).

- [ ] **Step 4: Implement** the helpers (top of `onedrive.rs`). Reuse `basename` from `objstore`; do **not** use `ObjPath` (OneDrive paths are real folder paths, not container/key):
  ```rust
  //! OneDrive for Business backend over the Microsoft Graph REST API.
  //! Unlike s3.rs/azblob.rs this drive has REAL folders, so we use Graph's
  //! path-addressed DriveItem URLs (`/me/drive/root:/path:` …) rather than the
  //! flat-namespace ObjPath synthesis. (graph/api/resources/onedrive)
  use crate::error::{Result, StorageError};
  use crate::objstore::basename;
  use crate::vfs::{Capabilities, Entry, EntryKind, StorageBackend};
  use async_trait::async_trait;

  /// Percent-encode each path segment but keep the `/` separators, per Graph
  /// path-addressing. Root ("" / "/") addresses `/me/drive/root` directly.
  fn encode_drive_path(path: &str) -> String {
      path.trim_matches('/')
          .split('/')
          .filter(|s| !s.is_empty())
          .map(|seg| urlencode_segment(seg))
          .collect::<Vec<_>>()
          .join("/")
  }
  // Minimal RFC 3986 segment encoder (space->%20, etc). A tiny hand-rolled
  // encoder avoids pulling in `urlencoding`; verify against Graph for `#?%`.
  fn urlencode_segment(s: &str) -> String { /* encode reserved bytes */ todo!() }

  /// `GET`-able item URL: "/" -> root, "/a/b" -> root:/a/b:
  pub(crate) fn item_url(base: &str, path: &str) -> String {
      let p = encode_drive_path(path);
      if p.is_empty() { format!("{base}/me/drive/root") }
      else { format!("{base}/me/drive/root:/{p}:") }
  }
  pub(crate) fn children_url(base: &str, path: &str) -> String {
      let p = encode_drive_path(path);
      if p.is_empty() { format!("{base}/me/drive/root/children") }
      else { format!("{base}/me/drive/root:/{p}:/children") }
  }
  ```
  Add `pub mod onedrive;` to `lib.rs`.

- [ ] **Step 5: Run** — `cargo test -p wonderblob-core onedrive` green. **Commit:** `git commit -m "feat(core): OneDrive Graph path-addressing helpers + reqwest/wiremock deps"`.

---

### Task 2: `TokenProvider` trait + static test provider

The backend stays testable by depending only on an injected token source.

**Files:** modify `onedrive.rs`.

- [ ] **Step 1: Failing test:**
  ```rust
  #[tokio::test]
  async fn static_token_provider_returns_token() {
      let p = StaticToken::new("abc");
      assert_eq!(p.access_token().await.unwrap(), "abc");
  }
  ```

- [ ] **Step 2: Implement:**
  ```rust
  /// Source of a currently-valid Graph bearer token. Injected so core stays
  /// testable (mock returns a fixed token; the real impl silently refreshes).
  #[async_trait]
  pub trait TokenProvider: Send + Sync {
      async fn access_token(&self) -> Result<String>;
  }

  /// Test/double: a fixed token (used by the wiremock suite).
  pub struct StaticToken(String);
  impl StaticToken { pub fn new(t: impl Into<String>) -> Self { Self(t.into()) } }
  #[async_trait]
  impl TokenProvider for StaticToken {
      async fn access_token(&self) -> Result<String> { Ok(self.0.clone()) }
  }
  ```

- [ ] **Step 3: Run** green. **Commit:** `git commit -m "feat(core): TokenProvider trait + StaticToken test double"`.

---

### Task 3: Token-endpoint calls in core (code exchange + refresh), mock-tested

Pure async fns over `reqwest`, parameterized by `auth_base_url` so `wiremock` can stand in. The interactive browser/loopback half lives in `src-tauri` (Task 11); the HTTP half lives here so it's covered without real auth.

**Files:** modify `onedrive.rs`.

- [ ] **Step 1: Failing test** (mock the `/token` endpoint):
  ```rust
  #[tokio::test]
  async fn refresh_posts_grant_and_parses_tokens() {
      use wiremock::{Mock, MockServer, ResponseTemplate, matchers::{method, path, body_string_contains}};
      let s = MockServer::start().await;
      Mock::given(method("POST")).and(path("/token"))
          .and(body_string_contains("grant_type=refresh_token"))
          .and(body_string_contains("client_id=CID"))
          .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
              "access_token": "AT", "refresh_token": "RT2", "expires_in": 3600
          })))
          .mount(&s).await;
      let r = refresh_tokens(&reqwest::Client::new(), &format!("{}", s.uri()), "CID", "RT1").await.unwrap();
      assert_eq!(r.access_token, "AT");
      assert_eq!(r.refresh_token.as_deref(), Some("RT2"));
      assert!(r.expires_in >= 3600);
  }
  ```

- [ ] **Step 2: Implement.** `auth_base_url` for production is `https://login.microsoftonline.com/organizations/oauth2/v2.0` (the test passes `{mock}` whose `/token` and `/authorize` it mounts):
  ```rust
  pub const SCOPES: &str = "Files.ReadWrite.All offline_access User.Read openid profile";

  #[derive(serde::Deserialize)]
  pub struct TokenResponse {
      pub access_token: String,
      #[serde(default)] pub refresh_token: Option<String>, // rotated; may be absent
      #[serde(default)] pub expires_in: i64,
      #[serde(default)] pub id_token: Option<String>,
  }

  /// PKCE authorization-code redemption (public client: NO client_secret).
  /// (entra/identity-platform/v2-oauth2-auth-code-flow#redeem-a-code-for-an-access-token)
  pub async fn exchange_code(
      client: &reqwest::Client, auth_base: &str, client_id: &str,
      code: &str, code_verifier: &str, redirect_uri: &str,
  ) -> Result<TokenResponse> {
      let form = [
          ("client_id", client_id), ("grant_type", "authorization_code"),
          ("code", code), ("code_verifier", code_verifier),
          ("redirect_uri", redirect_uri), ("scope", SCOPES),
      ];
      post_token(client, auth_base, &form).await
  }

  /// Refresh grant (public client). May rotate the refresh token.
  /// (…/v2-oauth2-auth-code-flow#refresh-the-access-token)
  pub async fn refresh_tokens(
      client: &reqwest::Client, auth_base: &str, client_id: &str, refresh_token: &str,
  ) -> Result<TokenResponse> {
      let form = [
          ("client_id", client_id), ("grant_type", "refresh_token"),
          ("refresh_token", refresh_token), ("scope", SCOPES),
      ];
      post_token(client, auth_base, &form).await
  }

  async fn post_token(client: &reqwest::Client, auth_base: &str, form: &[(&str, &str)]) -> Result<TokenResponse> {
      let resp = client.post(format!("{auth_base}/token")).form(form).send().await
          .map_err(net)?;
      let status = resp.status();
      if !status.is_success() {
          let body = resp.text().await.unwrap_or_default();
          // invalid_grant => refresh token expired/revoked => re-auth required.
          return Err(if body.contains("invalid_grant") || status.as_u16() == 400 {
              StorageError::AuthFailed { detail: format!("token endpoint {status}: {body}") }
          } else { StorageError::Network { detail: format!("token endpoint {status}") } });
      }
      resp.json::<TokenResponse>().await.map_err(StorageError::other)
  }

  fn net(e: reqwest::Error) -> StorageError { StorageError::Network { detail: e.to_string() } }
  ```

- [ ] **Step 3: Run** green. **Commit:** `git commit -m "feat(core): Graph token exchange + refresh (mock-tested)"`.

---

### Task 4: `RefreshingTokenProvider` (silent refresh + rotation persistence)

Caches the access token with expiry; refreshes on demand; persists a rotated refresh token via an injected callback (the keychain writer in src-tauri).

**Files:** modify `onedrive.rs`.

- [ ] **Step 1: Failing test** — first call hits `/token` (refresh) once, a second call within the expiry window reuses the cached access token (mock asserts exactly 1 call). Rotated `RT2` is delivered to the on-rotate callback.

- [ ] **Step 2: Implement:**
  ```rust
  use std::sync::Arc;
  use tokio::sync::Mutex;
  use std::time::{Duration, Instant};

  /// Holds the long-lived refresh token + a cached short-lived access token.
  /// On refresh, if Graph rotates the refresh token, `on_rotate` persists it.
  pub struct RefreshingTokenProvider {
      client: reqwest::Client,
      auth_base: String,
      client_id: String,
      inner: Mutex<RtState>,
      on_rotate: Arc<dyn Fn(String) + Send + Sync>, // keychain writer
  }
  struct RtState { refresh_token: String, access: Option<(String, Instant)> }

  impl RefreshingTokenProvider {
      pub fn new(client: reqwest::Client, auth_base: String, client_id: String,
                 refresh_token: String, on_rotate: Arc<dyn Fn(String)+Send+Sync>) -> Self {
          Self { client, auth_base, client_id,
                 inner: Mutex::new(RtState { refresh_token, access: None }), on_rotate }
      }
  }
  #[async_trait]
  impl TokenProvider for RefreshingTokenProvider {
      async fn access_token(&self) -> Result<String> {
          let mut st = self.inner.lock().await;
          if let Some((tok, exp)) = &st.access {
              if Instant::now() < *exp { return Ok(tok.clone()); }
          }
          let r = refresh_tokens(&self.client, &self.auth_base, &self.client_id, &st.refresh_token).await?;
          if let Some(new_rt) = r.refresh_token {
              if new_rt != st.refresh_token { (self.on_rotate)(new_rt.clone()); st.refresh_token = new_rt; }
          }
          // Refresh 60s early to avoid edge-of-expiry 401s.
          let ttl = Duration::from_secs((r.expires_in.max(60) as u64).saturating_sub(60));
          st.access = Some((r.access_token.clone(), Instant::now() + ttl));
          Ok(r.access_token)
      }
  }
  ```

- [ ] **Step 3: Run** green. **Commit:** `git commit -m "feat(core): RefreshingTokenProvider with cache + rotation callback"`.

---

### Task 5: `OneDriveBackend` skeleton + `list`

**Files:** modify `onedrive.rs`.

- [ ] **Step 1: Failing test** (wiremock). Mock `GET /me/drive/root/children` (root) → JSON `{ value: [ {name:"Documents", folder:{}, ...}, {name:"a.txt", file:{}, size:5, lastModifiedDateTime:"2024-01-02T03:04:05Z", eTag:"\"E1\""} ] }`; assert `list("/")` returns a Dir then File, sorted dirs-first, with `size`/`modified_ms` parsed and `path` = `/Documents`, `/a.txt`. Also mock the path-addressed children URL for `list("/Documents")`. Assert the request carried `Authorization: Bearer <token>`.

- [ ] **Step 2: Implement** config/struct + a request helper that attaches the bearer token, then `list`:
  ```rust
  pub struct OneDriveConfig { pub base_url: String, pub token: Arc<dyn TokenProvider> }
  pub struct OneDriveBackend { client: reqwest::Client, base_url: String, token: Arc<dyn TokenProvider> }

  #[derive(serde::Deserialize)]
  struct DriveItem {
      name: Option<String>,
      size: Option<i64>,
      #[serde(rename = "lastModifiedDateTime")] last_modified: Option<String>,
      #[serde(rename = "eTag")] etag: Option<String>,
      folder: Option<serde_json::Value>, // facet present => directory
      file: Option<serde_json::Value>,
      #[serde(rename = "@microsoft.graph.downloadUrl")] download_url: Option<String>,
      id: Option<String>,
  }
  #[derive(serde::Deserialize)]
  struct Children { value: Vec<DriveItem>, #[serde(rename="@odata.nextLink")] next: Option<String> }

  impl OneDriveBackend {
      pub fn new(cfg: OneDriveConfig) -> Self {
          Self { client: reqwest::Client::new(), base_url: cfg.base_url, token: cfg.token }
      }
      async fn bearer(&self) -> Result<String> {
          Ok(format!("Bearer {}", self.token.access_token().await?))
      }
  }
  ```
  - `list`: GET `children_url(base, path)`, attach bearer, follow `@odata.nextLink` pages (default page size 200), map each `DriveItem` → `Entry { kind: folder.is_some() ? Dir : File, size as u64, modified_ms = parse RFC3339 -> unix_ms, path = join(parent, name) }`. Sort dirs-first then case-insensitive name (mirror s3.rs/azblob.rs ordering). Parse ISO-8601 with the `time` crate (already a dep) via `OffsetDateTime::parse(.., &Rfc3339)`.

- [ ] **Step 3: Run** green. **Commit:** `git commit -m "feat(core): OneDriveBackend list over Graph children (mock-tested)"`.

---

### Task 6: `stat` (eTag capture) + `read` (ranged via downloadUrl)

**Files:** modify `onedrive.rs`.

- [ ] **Step 1: Failing tests:**
  - `stat("/a.txt")`: mock `GET …root:/a.txt:` → DriveItem with `file` facet, `size`, `eTag` → assert `Entry{kind:File,size,modified_ms}`. Root `stat("/")` → mock `GET …/root` → Dir. `404` → `NotFound`.
  - `read("/a.txt", 0)`: mock `GET …root:/a.txt:/content` → 200 body `"hello"`; assert reader yields `hello`.
  - `read("/a.txt", 2)` (ranged): mock `GET …root:/a.txt:` → DriveItem with `@microsoft.graph.downloadUrl` = `{mock}/dl/a`; mock `GET /dl/a` with header `Range: bytes=2-` → `206` body `"llo"`; assert reader yields `llo`.

- [ ] **Step 2: Implement:**
  - `stat`: GET `item_url`; on success map facet→kind, capture `etag` into a private helper `stat_item(path) -> Result<DriveItem>` (reused by `rename`/conflict). Map non-2xx via `map_graph` (Task 9).
  - `read`: for `offset == 0`, `GET item_url + "/content"` (reqwest follows the 302 to the pre-authed downloadUrl; Range absent). For `offset > 0`, **do not** put `Range` on `/content` — instead `GET item_url` to read `@microsoft.graph.downloadUrl`, then `GET download_url` with header `Range: bytes={offset}-` expecting `206`. (Grounded: `driveitem-get-content#partial-range-downloads` — "append the Range header to the actual downloadUrl, not the request for /content".) Wrap the byte stream as `Box<dyn AsyncRead>` via `tokio_util::io::StreamReader` over `resp.bytes_stream()` (same shape azblob.rs uses).

- [ ] **Step 3: Run** green. **Commit:** `git commit -m "feat(core): OneDrive stat (eTag) + ranged read via downloadUrl"`.

---

### Task 7: `write` — small PUT vs resumable upload session (`AsyncWrite`, finalize in `poll_shutdown`)

**Trait constraint (unchanged):** `write(&self, path) -> Result<Box<dyn AsyncWrite + Send + Unpin>>`; the contract/transfer callers do `write_all(..).await` then `shutdown().await`, so the upload **must finalize inside `poll_shutdown`** — exactly like `S3MultipartWriter`/`AzBlockWriter`.

**The size problem (why OneDrive differs from S3/Azure):** Graph upload-session fragments require `Content-Range: bytes a-b/{TOTAL}` — the **total size up front** — which S3 multipart / Azure block-list do **not** need. A streaming `AsyncWrite` doesn't know the total until shutdown. **Decision:** the writer **spills incoming bytes to a `tempfile::NamedTempFile`** while counting bytes; on `poll_shutdown` the total is known, so we choose:
- **≤ 4 MiB:** single `PUT …:/content` of the whole buffer (recommended small-upload path).
- **> 4 MiB:** `POST …:/createUploadSession`, then stream the temp file in **10 MiB fragments (a 320 KiB multiple)** with correct `Content-Range`; the **last fragment is the remainder** (any size). Sequential, in order. This keeps memory bounded and is honestly resumable-shaped (the session URL + nextExpectedRanges enable Plan-3-style resume later). Trade-off vs S3/Azure noted in a code comment.

**Files:** modify `onedrive.rs`.

- [ ] **Step 1: Failing tests** (wiremock):
  - **Small:** `write("/a.txt")`, `write_all(b"hi")`, `shutdown()` → mock `PUT …root:/a.txt:/content` body `hi` → `201`. Assert the PUT fired (and **no** createUploadSession).
  - **Large/chunked:** write `700 KiB` (forces a session; use a small `SMALL_MAX` override in tests if needed, or write >4 MiB). Mock `POST …root:/big.bin:/createUploadSession` → `{ uploadUrl: "{mock}/upload/1" }`; mock `PUT /upload/1` matching `Content-Range` headers, intermediate fragments multiples of 320 KiB, final fragment = remainder → `202 Accepted` for intermediates and `201 Created` for the last. Assert the chunk sizes are 320 KiB multiples (except the last) and the last `Content-Range` ends at `total-1/total`.
  - **Empty file:** `write_all(b"")` + shutdown → single `PUT …/content` with empty body → `201`.

- [ ] **Step 2: Implement** `GraphUploadWriter` mirroring `S3MultipartWriter`'s `WState` machine (boxed in-flight future ⇒ `Unpin`), but spilling to temp:
  ```rust
  const SMALL_MAX: u64 = 4 * 1024 * 1024;          // ≤4 MiB => simple PUT
  const FRAGMENT: u64 = 10 * 1024 * 1024;          // 10 MiB == 32 * 320 KiB (a 320 KiB multiple)
  // Graph requires each non-final upload fragment to be a multiple of 320 KiB
  // (327680 B); the final fragment may be any size. (driveitem-createuploadsession)
  ```
  - `poll_write`: append `data` to the temp file (via a buffered `std::fs::File`/`tokio::fs`), `total += len`, return `Ready(len)`.
  - `poll_shutdown`: drive a state machine: `Idle` → if `total <= SMALL_MAX` start `SimplePut` (read whole temp file → `PUT item_url+"/content"`); else start `CreateSession` (`POST …createUploadSession` with `{ item: { @microsoft.graph.conflictBehavior: "replace", name } }`), then loop `UploadFragment` reading `FRAGMENT`-sized slices from the temp file at the running offset, sending `Content-Range: bytes {off}-{off+len-1}/{total}`, until the last (smaller) fragment; map `202` continue / `201|200` done. On `404` during fragments → `StorageError::Conflict`/restart note (v1: surface as error). `Done` → `Ready(Ok)`.
  - On any error mid-session, best-effort `DELETE {uploadUrl}` (cancel) like S3's abort — fire-and-forget.

- [ ] **Step 3: Run** green. **Commit:** `git commit -m "feat(core): OneDrive write — small PUT + resumable upload session (poll_shutdown)"`.

---

### Task 8: `delete`, `rename`/move (`if-match` eTag), `mkdir`

**Files:** modify `onedrive.rs`.

- [ ] **Step 1: Failing tests:**
  - `delete("/a.txt")` → mock `DELETE …root:/a.txt:` → `204`. `404`→`NotFound`.
  - `mkdir("/New")` → mock `POST …/root/children` (parent = root) body `{name:"New", folder:{}, @microsoft.graph.conflictBehavior:"fail"}` → `201`. Nested `mkdir("/A/B")` posts to `…root:/A:/children`.
  - `rename("/a.txt","/sub/b.txt")` → mock `GET …root:/a.txt:` (read id+eTag) → DriveItem `{id:"I1", eTag:"E1"}`; mock `GET …root:/sub:` → folder `{id:"P2"}`; mock `PATCH …/items/I1` with header `if-match: E1` body `{name:"b.txt", parentReference:{id:"P2"}}` → `200`. A `412` response → `StorageError::Conflict`.

- [ ] **Step 2: Implement:**
  - `delete`: `DELETE item_url`. (Graph deletes folders recursively to recycle bin; note in a comment this differs from S3/Azure's non-empty-dir `Conflict` — OneDrive has real recursive delete, accepted for v1.)
  - `mkdir`: split parent/leaf via `basename` + parent path; `POST children_url(parent)` with the folder body, `conflictBehavior:"fail"` so an existing dir → `409` → `Conflict`.
  - `rename`: `stat_item(from)` for `{id, etag}`; if dest parent differs, `stat_item(parent_of(to))` for its `id`; `PATCH base/me/drive/items/{id}` with `if-match:{etag}` and body `{ name, parentReference:{ id } }`. Use **id-addressing** for the PATCH (move endpoint is id-based; `parentReference.id` required — root moves need the real root id, not `"root"`, per `driveitem-move`). Map `412`→`Conflict`.

- [ ] **Step 3: Run** green. **Commit:** `git commit -m "feat(core): OneDrive delete/mkdir/rename(move) with if-match conflict"`.

---

### Task 9: `share_link`, capabilities, Graph error mapping

**Files:** modify `onedrive.rs`.

- [ ] **Step 1: Failing tests:**
  - `share_link("/a.txt", 86400)` → mock `POST …root:/a.txt:/createLink` body containing `"type":"view"` and `"scope":"organization"` → `201 {link:{webUrl:"https://contoso-my.sharepoint.com/..."}}`; assert the returned URL. Also assert an `expirationDateTime` ~24h out is sent.
  - Error mapping: `403`→`PermissionDenied`, `404`→`NotFound`, `429`/`5xx`→`Network` (retryable), `412`→`Conflict`, `401`→`AuthFailed`.

- [ ] **Step 2: Implement:**
  ```rust
  fn capabilities(&self) -> Capabilities {
      // OneDrive has real folders + native sharing links + eTag conflict.
      Capabilities { can_presign: true, can_rename: true, can_set_mtime: false }
  }
  async fn share_link(&self, path: &str, expiry_secs: u64) -> Result<String> {
      // type=view + scope=organization: org-scoped by default for OneDrive for
      // Business; `anonymous` may be admin-disabled. (driveitem-createlink)
      let body = serde_json::json!({
          "type": "view", "scope": "organization",
          "expirationDateTime": iso8601_in(expiry_secs as i64),
      });
      let resp = self.client.post(format!("{}/createLink", item_url(&self.base_url, path)))
          .header("Authorization", self.bearer().await?).json(&body).send().await.map_err(net)?;
      // 201 new / 200 existing both OK.
      let perm: Permission = ok_json(path, resp).await?;
      perm.link.and_then(|l| l.web_url).ok_or_else(|| StorageError::Other { detail: "no link.webUrl".into() })
  }
  ```
  And `map_graph(path, status, body) -> StorageError` (heuristic on status + Graph error `code`, mirroring `map_s3`/`map_az` style): `401`→AuthFailed, `403`→PermissionDenied, `404`→NotFound, `409`→Conflict, `412`→Conflict, `429`/`>=500`→Network, else Other. `iso8601_in` reuses the `time` formatting pattern from azblob.rs's `expiry_in`.

- [ ] **Step 3: Run** green; full `StorageBackend` impl now compiles. **Commit:** `git commit -m "feat(core): OneDrive share_link (createLink), capabilities, Graph error map"`.

---

### Task 10: Core mock-Graph integration test (VFS-shaped coverage)

A single `tests/onedrive_mock.rs` integration test that stands up one `wiremock` server impersonating a small Graph drive and drives the backend through list→stat→read→write→rename→delete→mkdir→share_link, asserting the right Graph endpoints/verbs/headers are hit and responses parse into the VFS taxonomy. This is OneDrive's **CI coverage** (it does **not** join the Docker contract suite — Graph isn't self-hostable).

**Files:** create `crates/wonderblob-core/tests/onedrive_mock.rs`.

- [ ] **Step 1:** Build the test with `StaticToken::new("T")` + `base_url = server.uri()`; register Mocks for each operation; assert end-to-end. Include the chunked-upload assertions (320 KiB multiples + final remainder + `Content-Range`) and a `412`→`Conflict` rename case.
- [ ] **Step 2: Run** — `cargo test -p wonderblob-core --test onedrive_mock` green; runs with **no network** and **no env flag** (mock is in-process).
- [ ] **Step 3: Commit** `git commit -m "test(core): mock-Graph VFS coverage for OneDrive backend"`.

---

### Task 11: `src-tauri` OAuth interactive flow + connection plumbing

The non-headless half: PKCE, system browser, loopback listener, and the `connect_onedrive`/`connect_bookmark` wiring with keychain-stored refresh tokens.

**Files:**
- Create: `src-tauri/src/onedrive_auth.rs`
- Modify: `src-tauri/src/lib.rs` (`mod onedrive_auth;` + register `connect_onedrive`)
- Modify: `src-tauri/src/bookmarks.rs` (`Protocol::OneDrive`, `OneDriveParams`, `Bookmark.onedrive`)
- Modify: `src-tauri/src/commands.rs` (`connect_onedrive`, `connect_bookmark` arm)

- [ ] **Step 1: Bookmark model.** In `bookmarks.rs`:
  ```rust
  pub enum Protocol { Sftp, S3, AzBlob, OneDrive }
  #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
  #[serde(rename_all = "camelCase")]
  pub struct OneDriveParams {
      /// Per-connection client-ID override (spec); None => DEFAULT_CLIENT_ID.
      pub client_id_override: Option<String>,
      /// Display label from the id_token / `/me` (email or name) — metadata only.
      pub account_label: Option<String>,
  }
  // Add to Bookmark: `#[serde(default, skip_serializing_if="Option::is_none")] pub onedrive: Option<OneDriveParams>`
  ```
  The keychain secret for a OneDrive bookmark is the **refresh token** (not user-entered) — stored via the existing `secrets::set(bookmark_id, refresh_token)`. Add a test asserting an old SFTP/S3/Azure `bookmarks.json` still deserializes with `onedrive: None`, and a OneDrive bookmark round-trips with **no token in the file**.

- [ ] **Step 2: OAuth module.** In `onedrive_auth.rs`:
  ```rust
  //! Interactive OAuth (auth-code + PKCE, system browser, loopback listener).
  //! The HTTP token calls live in core (onedrive::{exchange_code,refresh_tokens});
  //! this module owns only the non-headless browser/listener half.
  use base64::Engine as _;
  use sha2::{Digest, Sha256};

  /// SHIPPED multi-tenant public client ID. *** PLACEHOLDER — replace with the
  /// Application (client) ID from Jack's Entra registration (see HUMAN SETUP). ***
  pub const DEFAULT_CLIENT_ID: &str = "PLACEHOLDER_CLIENT_ID";
  /// Work/school accounts only (OneDrive for Business). `common` would also allow
  /// personal accounts, which are explicitly deferred.
  pub const AUTH_BASE: &str = "https://login.microsoftonline.com/organizations/oauth2/v2.0";

  fn pkce() -> (String, String) { // (verifier, challenge S256)
      let verifier: String = /* 64 url-safe random chars */;
      let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
          .encode(Sha256::digest(verifier.as_bytes()));
      (verifier, challenge)
  }

  pub struct LoginResult { pub access_token: String, pub refresh_token: String, pub account_label: Option<String> }

  /// Run the full interactive flow. Opens the system browser to the authorize
  /// URL, catches the redirect on a one-shot 127.0.0.1 listener, exchanges the
  /// code for tokens. `client_id` defaults to DEFAULT_CLIENT_ID.
  pub async fn interactive_login(app: &tauri::AppHandle, client_id: &str) -> Result<LoginResult, StorageError> {
      // 1. Bind an ephemeral loopback port; redirect_uri = http://localhost:{port}
      //    (registered as http://localhost; port ignored for loopback matching).
      let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.map_err(StorageError::other)?;
      let port = listener.local_addr().map_err(StorageError::other)?.port();
      let redirect = format!("http://localhost:{port}");
      let (verifier, challenge) = pkce();
      let state = /* random */;
      // 2. authorize URL
      let url = format!("{AUTH_BASE}/authorize?client_id={client_id}&response_type=code\
          &redirect_uri={enc_redirect}&response_mode=query&scope={enc_scopes}\
          &code_challenge={challenge}&code_challenge_method=S256&state={state}");
      // 3. open system browser (tauri-plugin-opener, already a dep)
      use tauri_plugin_opener::OpenerExt;
      app.opener().open_url(url, None::<&str>).map_err(StorageError::other)?;
      // 4. accept ONE connection; parse `code`/`state` from the GET request line;
      //    write a tiny "You can close this tab" 200 HTML response. Validate state.
      let (code, got_state) = accept_one_redirect(listener).await?; // hand-rolled, ~30 lines
      if got_state != state { return Err(StorageError::AuthFailed { detail: "state mismatch".into() }); }
      // 5. exchange code -> tokens (core)
      let client = reqwest::Client::new();
      let tr = wonderblob_core::onedrive::exchange_code(&client, AUTH_BASE, client_id, &code, &verifier, &redirect).await?;
      let refresh_token = tr.refresh_token.ok_or(StorageError::AuthFailed { detail: "no refresh token (offline_access?)".into() })?;
      let account_label = tr.id_token.as_deref().and_then(account_label_from_id_token); // decode JWT payload `preferred_username`/`name`
      Ok(LoginResult { access_token: tr.access_token, refresh_token, account_label })
  }
  ```
  `accept_one_redirect` reads the first request line (`GET /?code=...&state=... HTTP/1.1`), URL-decodes the query, returns `(code, state)`, and replies `HTTP/1.1 200 OK\r\n\r\n<html>…close this tab…</html>`. A `15-minute` overall timeout (`tokio::time::timeout`) guards an abandoned browser. `account_label_from_id_token` base64-decodes the JWT payload segment (no signature verification needed — it's display metadata only) and reads `preferred_username`/`name`.

- [ ] **Step 3: Commands.** In `commands.rs`:
  ```rust
  #[derive(Deserialize)]
  #[serde(rename_all = "camelCase")]
  pub struct OneDriveConnectArgs { pub bookmark_id: uuid::Uuid, pub client_id_override: Option<String> }

  #[derive(serde::Serialize)]
  #[serde(rename_all = "camelCase")]
  pub struct OneDriveConnectResult { pub id: ConnectionId, pub capabilities: Capabilities, pub account_label: Option<String> }

  /// Interactive sign-in: runs OAuth in the system browser, stores the refresh
  /// token in the keychain under `bookmark_id`, registers a OneDrive backend.
  #[tauri::command]
  pub async fn connect_onedrive(app: tauri::AppHandle, state: State<'_, AppState>, args: OneDriveConnectArgs)
      -> Result<OneDriveConnectResult, StorageError> {
      let client_id = args.client_id_override.clone()
          .unwrap_or_else(|| crate::onedrive_auth::DEFAULT_CLIENT_ID.to_string());
      // No CONNECT_TIMEOUT here — the user may take a while in the browser; the
      // OAuth module enforces its own 15-min cap.
      let login = crate::onedrive_auth::interactive_login(&app, &client_id).await?;
      let key = args.bookmark_id.to_string();
      let k = key.clone(); let rt = login.refresh_token.clone();
      keychain(move || secrets::set(&k, &rt)).await?;
      let backend = build_onedrive_backend(&app, &key, &client_id, login.refresh_token);
      let mut res = register(&state, backend).await;
      Ok(OneDriveConnectResult { id: res.id, capabilities: res.capabilities, account_label: login.account_label })
  }
  ```
  `build_onedrive_backend` constructs a `RefreshingTokenProvider` whose `on_rotate` callback writes the rotated refresh token back to the keychain (spawn_blocking) under the same key, then `OneDriveBackend::new(OneDriveConfig{ base_url: "https://graph.microsoft.com/v1.0", token })`. Add the **`connect_bookmark` OneDrive arm**: load the refresh token from the keychain; if absent → `AuthFailed{detail:"sign in again"}` (UI re-runs `connect_onedrive`); else build the backend (silent refresh happens on first request); register. Add `connect_onedrive` to the `invoke_handler!` list and `mod onedrive_auth;` in `lib.rs`.

- [ ] **Step 4:** `cargo build --workspace` + `cargo test -p wonderblob src-tauri` (bookmark serde tests) green. **Commit:** `git commit -m "feat(tauri): OneDrive OAuth (PKCE/loopback) + connect commands + keychain refresh tokens"`.

---

### Task 12: Frontend — ConnectionSheet OneDrive mode + "Sign in with Microsoft"

OneDrive's sheet mode has **no host/key/secret fields** — it's OAuth-button-driven. The field-swap must handle an auth flow rather than a secret.

**Files:** modify `src/lib/api.ts`, `src/lib/components/ConnectionSheet.svelte`, `src/lib/components/BookmarkList.svelte`.

- [ ] **Step 1: api.ts.** Extend `Protocol = "sftp" | "s3" | "azBlob" | "oneDrive"`; add `OneDriveParams { clientIdOverride: string | null; accountLabel: string | null }`; add `onedrive?: OneDriveParams | null` to `Bookmark`; add:
  ```ts
  export interface OneDriveConnectResult { id: number; capabilities: Capabilities; accountLabel: string | null }
  connectOnedrive: (bookmarkId: string, clientIdOverride?: string | null) =>
    invoke<OneDriveConnectResult>("connect_onedrive", { args: { bookmarkId, clientIdOverride: clientIdOverride ?? null } }),
  ```

- [ ] **Step 2: ConnectionSheet.** Add `<option value="oneDrive">OneDrive for Business</option>`. Add OneDrive state (`odClientId`, `odAccountLabel`, generated `odBookmarkId = bookmark?.id ?? crypto.randomUUID()`, `signedIn`). In the OneDrive branch render: Label, an advanced collapsible "Custom client ID (optional)" input, optional Initial path, and a **"Sign in with Microsoft"** button. The button calls `api.connectOnedrive(odBookmarkId, odClientId || null)`; on success it stores `accountLabel`, marks `signedIn=true`, sets `activeConnection`, and **saves the bookmark** (`api.bookmarkSave(buildBookmark(odBookmarkId), undefined)` — no secret arg; the refresh token was already written to the keychain under `odBookmarkId` by the command). `buildBookmark` gains a OneDrive arm returning `{ protocol:"oneDrive", onedrive:{ clientIdOverride: odClientId||null, accountLabel: odAccountLabel||null }, initialPath: initialPath || "/" }`. `protoUsesSecret` returns **false** for OneDrive (no secret field renders). `valid()` for OneDrive requires only a label (sign-in is the real gate). Save flow: for OneDrive, "Save" without sign-in just persists metadata; the button is the connect path.

- [ ] **Step 3: BookmarkList.** `protoBadge`: add `p === "oneDrive" ? "OneDrive"`. `rowTitle`: OneDrive → `b.onedrive?.accountLabel ?? "OneDrive for Business"`. `connect(b)` for a saved OneDrive bookmark calls `api.connectBookmark(b.id)` (silent refresh); if it throws `authFailed`, surface "Sign in again" (the user re-opens the sheet → Sign in with Microsoft).

- [ ] **Step 4:** `npm run check` + `npm test` green. **Commit:** `git commit -m "feat(ui): OneDrive connection mode with Sign in with Microsoft + badge"`.

- [ ] **Step 5: Interactive smoke (BLOCKED on Jack's client ID).** Once `DEFAULT_CLIENT_ID` is set: `npm run tauri dev` → New Connection → OneDrive for Business → Sign in with Microsoft → browser opens → consent → "close this tab" page → sheet shows the account label → root lists the user's OneDrive folders → open/preview a file → Share Link copies an org `webUrl` that resolves → disconnect/reconnect uses silent refresh (no browser). Record results in the manual checklist (Task 13).

---

### Task 13: Manual smoke checklist + env-gated real-tenant test; CI note

Graph isn't self-hostable, so CI coverage = the in-process `wiremock` tests (Task 10), which run on plain `cargo test` with no fixture/secret. A real-tenant test is **interactive** (OAuth) and stays out of CI; it runs only with a real refresh token in env.

**Files:** create `crates/wonderblob-core/tests/onedrive_live.rs`; modify `.github/workflows/ci.yml` (comment only); add `docs/...` checklist inline in the PR description (no new md file required by the plan).

- [ ] **Step 1: Gated live test.** `onedrive_live.rs` skips unless `WONDERBLOB_TEST_ONEDRIVE=1` **and** `WONDERBLOB_ONEDRIVE_REFRESH_TOKEN` + `WONDERBLOB_ONEDRIVE_CLIENT_ID` are set. When present: build a `RefreshingTokenProvider` from the env refresh token against the **real** `login.microsoftonline.com/organizations/oauth2/v2.0`, point `OneDriveBackend` at real Graph, run a write→stat→read→share_link→delete round-trip under a `/wonderblob-test/` folder. Default `cargo test` → skipped (prints a skip line). (Mirrors the `WONDERBLOB_TEST_S3`/`_AZBLOB` gating pattern.)
- [ ] **Step 2: CI.** Add a comment in `ci.yml` near the S3/Azure fixture steps: OneDrive has **no Docker fixture** (Graph not self-hostable); its CI coverage is the in-process `--test onedrive_mock` suite, which already runs in the existing `cargo test --workspace` step. No new secret/fixture.
- [ ] **Step 3: Manual checklist** (put in PR body): interactive sign-in opens system browser; consent prompts the three scopes; redirect lands on the loopback page; account label appears; list/stat/read/preview/open-edit/save-back; Share Link resolves; large-file (>4 MiB) upload via session completes and re-downloads byte-identical; reconnect uses silent refresh; revoking the refresh token (or after expiry) surfaces "sign in again" rather than a crash.
- [ ] **Step 4:** `cargo test --workspace` green (live test skipped). **Commit:** `git commit -m "test(core): env-gated OneDrive live round-trip + CI note (no fixture)"`.

---

## Done criteria (Plan 5)

- `cargo test --workspace` + `npm run check` + `npm test` green locally and in CI, **with no network and no new Docker fixture/secret** (the `onedrive_mock` wiremock suite is the CI coverage).
- `OneDriveBackend` implements the unchanged `StorageBackend` trait; `list`/`stat`/`read`/`write`/`delete`/`rename`/`mkdir`/`share_link` map to the correct path-addressed Graph endpoints and parse DriveItem responses into the existing `Entry`/`Capabilities`/`StorageError` taxonomy.
- `list("/")` shows the user's real OneDrive root folders (real folders, not synthesized buckets).
- Resumable upload: files ≤4 MiB use a single `PUT …/content`; larger files use `createUploadSession` with 320 KiB-multiple fragments + correct `Content-Range`, finalizing inside `poll_shutdown`; empty-file and exact-boundary cases covered.
- eTag: `rename`/move sends `if-match` and maps `412` → `Conflict` (a real concurrency guard S3/Azure lack).
- OAuth: PKCE (S256) auth-code flow in the **system browser** via a one-shot 127.0.0.1 loopback listener (zero new deps); public client (no secret); `Files.ReadWrite.All offline_access User.Read` scopes; refresh token persisted in the OS keychain under the bookmark UUID; access token cached in-memory with silent refresh; rotated refresh tokens re-persisted.
- Connect flow: first-time = interactive (`connect_onedrive` opens the browser); returning = silent (`connect_bookmark` refreshes); refresh failure surfaces `AuthFailed` → "sign in again", never a crash.
- Frontend: OneDrive protocol option with no host/key fields, an optional custom-client-ID advanced field, a "Sign in with Microsoft" button driving the OAuth flow, a "OneDrive" badge, and capability-gated Share Link (`canPresign=true`).
- `DEFAULT_CLIENT_ID` is a clearly-marked placeholder until Jack supplies the real Application (client) ID; only Task 12-smoke and Task 13-live are blocked on it.

## Explicitly deferred

- **SharePoint document libraries / Groups drives** (`/sites/{id}/drive`, `/groups/{id}/drive`) — v1 is the signed-in user's personal OneDrive for Business (`/me/drive`) only.
- **Personal (consumer) OneDrive accounts** — `organizations` authority targets work/school only; `common`/personal-MSA support, embed links, and password-protected links are out.
- **Large-tenant throttling depth** — we map `429`/`5xx` to retryable `Network` (TransferEngine backoff applies), but full `Retry-After` header honoring and adaptive concurrency are deferred.
- **Delta sync** (`/delta`), server-side copy, recursive move of non-empty folders with progress, and `nextExpectedRanges`-based mid-file upload **resume** persistence (the session is resumable-shaped but Plan-3 chunk-state persistence for Graph is not wired).
- **EditSession eTag wiring** — the backend captures `eTag`, but threading it into EditSession's conflict guard (beyond the existing mtime/size check) is a follow-up.
- **127.0.0.1 manifest-based redirect** (vs portal `http://localhost`) and custom-URI-scheme (`wonderblob://auth`) / `nativeclient` redirect strategies — loopback is the shipped choice.
- **MSAL** — we hand-roll the minimal flow (no `msal` Rust crate dependency; none is first-party/mature) per the "lightest" constraint.

## Self-review (writing-plans checklist)

- **Spec coverage:** OAuth PKCE in system browser ✓ (Task 11, loopback listener, no device code, no secret); shipped multi-tenant public client ID + per-connection override ✓ (`DEFAULT_CLIENT_ID` + `clientIdOverride`); Graph upload sessions for resumable large files ✓ (Task 7); native sharing links ✓ (Task 9 `createLink`); refresh tokens in keychain ✓ (Task 11 via `secrets`); backend row + auth row of the spec both addressed. Mocked-Graph CI + manual smoke checklist ✓ (Tasks 10/13), matching the spec's Testing section ("Graph-mock layer for CI plus a manual smoke checklist").
- **No placeholders/hand-waving in process:** every task has a failing test, an implement step with real code shapes, run commands, and a commit. The **one intentional placeholder** is `DEFAULT_CLIENT_ID` (gated, clearly flagged, with the exact Entra steps to resolve it) — required because only Jack can register the app. `urlencode_segment`/`accept_one_redirect`/`pkce` bodies are sketched with `todo!()`/comments where the mechanism is described but the byte-level code is the implementer's to fill — flagged, not hidden.
- **Type/name consistency with REAL symbols (verified against the files):** `StorageBackend` trait surface unchanged (`read(path, offset)`, `write(path)->Box<dyn AsyncWrite+Send+Unpin>`, `share_link(path, expiry_secs)`); `Entry{name,path,kind,size:Option<u64>,modified_ms:Option<i64>}`, `EntryKind::{File,Dir}`, `Capabilities{can_presign,can_rename,can_set_mtime}`, `StorageError::{AuthFailed,NotFound,PermissionDenied,Network,Conflict,Unsupported,Other}` + `StorageError::other`, `Result<T>` all used exactly. `objstore::basename` reused; `ObjPath`/`PART_SIZE` deliberately **not** reused (real folders). Tauri layer reuses `AppState`/`ConnectionId`/`register`/`ConnectResult`/`keychain`/`bookmarks::secrets`/`store`, and follows the `connect_*` + `connect_bookmark` arm pattern; `Protocol` extended with `OneDrive`; `Bookmark` extended with `onedrive: Option<OneDriveParams>` exactly like `s3`/`azblob`. Frontend reuses `api.ts` `invoke` pattern, `Protocol` union, `protoBadge`/`rowTitle`/field-swap, and `crypto.randomUUID()` bookmark-id convention from `ConnectionSheet.svelte`.
- **HUMAN SETUP callout:** present and prominent (top of plan) with the exact Entra clicks; the Jack-blocked tasks (12-smoke, 13-live) vs the placeholder-proceeds tasks (1–11, 12 code, 10 mock CI) are explicitly separated.
- **Research-backed:** every Graph endpoint, scope, redirect-URI rule, refresh grant, and the 320 KiB fragment rule is cited to a Microsoft Learn URL in the facts table and inline comments (grounded via the Microsoft Learn MCP, not guessed).
