//! `JsonApiAccess`: authenticated JSON requests (dispatch VA 0x0080da7c, headers 0x0080c254,
//! transport 0x0080ccf8, error reply 0x004611d4) and account calls (`user/signin` 0x0030c768,
//! `user/signout` 0x0030b3c4, `user/profile` 0x00301b94, `user/refresh_token` 0x0044da14).

use std::time::Duration;

use serde_json::{Value, json};
use thiserror::Error;

use super::DeviceIdentity;
use super::endpoint::CloudEndpoint;
use super::session::{Claims, MAX_REFRESH_FAILURES, TokenStore, TokenStoreError, Tokens};
use crate::exec::Secret;
use crate::http::{HttpClient, HttpError, HttpRequest, Method, Route};

/// Stock request default (VA 0x00310a74).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(30_000);

/// A cloud error reply `{"error": <int>, "message": "...", "requestId": "..."}`. A missing key or
/// a non-numeric `error` gives code -1, as stock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudError {
    pub http_status: u16,
    pub code: i64,
    pub message: Option<String>,
    pub request_id: Option<String>,
}

impl CloudError {
    pub fn from_reply(http_status: u16, body: &[u8]) -> Self {
        let reply: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
        let text = |key: &str| reply.get(key).and_then(Value::as_str).map(str::to_owned);
        Self {
            http_status,
            code: reply.get("error").and_then(Value::as_i64).unwrap_or(-1),
            message: text("message"),
            request_id: text("requestId"),
        }
    }

    /// A downloaded product that failed its check (`0x45fa44`): code -20, the log text
    /// "Downloaded product %1 (%2) is corrupt." (I: `%1` the name, `%2` the id).
    pub fn corrupt_product(name: &str, id: &str) -> Self {
        Self {
            http_status: 0,
            code: -20,
            message: Some(format!("Downloaded product {name} ({id}) is corrupt.")),
            request_id: None,
        }
    }

    /// The text ZenUI shows for this error.
    pub fn text(&self) -> String {
        stock_message(self.code).map_or_else(
            || {
                format!(
                    "Something went wrong. Please try again. If this continues to happen, please contact our support team, citing: \"Error code {}\"",
                    self.code
                )
            },
            str::to_owned,
        )
    }
}

/// Error texts from the static map at `_INIT_354` (VA 0x0008d3c0). Codes 4, 5 and -20 are
/// client-side. Validation codes 2002–2076 beyond those listed are not mapped here.
pub fn stock_message(code: i64) -> Option<&'static str> {
    Some(match code {
        4 => "Operation timed out",
        5 => "Operation canceled",
        -20 => "This product is corrupt",
        301 => {
            "Cloud server/protocol error. Please try again. If this continues to happen, please contact our support team."
        }
        2000 | 3100 | 3200 | 3201 => {
            "Something went wrong. Please try again. If this continues to happen, please contact our support team."
        }
        2001 => "Please enter a valid email address.",
        2050 => "Product name contains invalid characters.",
        2051 => {
            "Product name must be between 4 and 20 characters (for Preset and Neural Capture) or between 4 and 60 characters (for Impulse Response)."
        }
        3101 => "Requested item not found on cloud.",
        3102 => "Item already exists.",
        3104 => "You have exceeded your upload limit.",
        3105 => "This username is unavailable.",
        3106 => "This email is unavailable.",
        3202..=3204 => {
            "Incorrect email address or password. Please try again.<br><br>If you need to reset your password, visit neuraldsp.com/reset-password"
        }
        3205 => "Invalid access token. Please re-enter your Neural account credentials.",
        3206 => "Too many failed login attempts. Please try again in a few minutes.",
        3207 => {
            "Due to security reasons, you cannot use your current password. If you are creating a new account, please choose a different one. If you are signing in, please change your password as soon as possible."
        }
        _ => return None,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ApiError {
    #[error("not signed in")]
    NotSignedIn,
    /// More than [`MAX_REFRESH_FAILURES`] refreshes failed; tokens were cleared.
    #[error("signed out after repeated token refresh failures")]
    SignedOut,
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error("cloud error {}: {}", .0.code, .0.text())]
    Cloud(CloudError),
    #[error("unexpected cloud reply: {0}")]
    Reply(String),
    #[error(transparent)]
    Store(#[from] TokenStoreError),
    #[error(transparent)]
    Sign(#[from] super::SignError),
    #[error("{0} is not available with a custom cloud endpoint")]
    NoHost(&'static str),
    /// An id refused before any request (review r32 E2, `cloud::services` `path_segment`).
    #[error("{0:?} cannot name a cloud resource (empty, dots only or with a '/')")]
    BadId(String),
    /// A backup name refused before `generateBackupUrl/<name>` (review r33 F6, O: stock puts
    /// the name in as it is, `0x303300`).
    #[error("the backup name {0:?} cannot be used: it may not contain '/' or be dots only")]
    BadBackupName(String),
}

/// Send and decode: 2xx gives the JSON body (`Null` when empty), anything else a [`CloudError`].
pub fn exchange(http: &mut impl HttpClient, request: &HttpRequest) -> Result<Value, ApiError> {
    let response = http.send(request)?;
    if !response.is_success() {
        return Err(ApiError::Cloud(CloudError::from_reply(
            response.status,
            &response.body,
        )));
    }
    if response.body.iter().all(u8::is_ascii_whitespace) {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&response.body).map_err(|e| ApiError::Reply(e.to_string()))
}

/// Cloud session state: endpoint, identity, tokens and the refresh failure count. It holds no
/// backend; each call borrows the HTTP client and the [`TokenStore`], so the platform keeps
/// ownership of them.
#[derive(Debug)]
pub struct CloudApi {
    pub endpoint: CloudEndpoint,
    pub device: DeviceIdentity,
    /// Requests travel through Cortex Control (ProductForward). Stock then sends
    /// `Hardware-Source-*` headers instead of `Client-*` ones (`0x80c254`). The caller sets it
    /// per call: tetrad-app's cloud job while it runs over Control.
    pub via_control: bool,
    tokens: Option<Tokens>,
    username: Option<String>,
    refresh_failures: u32,
}

impl CloudApi {
    /// Build the session with the saved tokens and username, if any.
    pub fn load(
        store: &mut impl TokenStore,
        endpoint: CloudEndpoint,
        device: DeviceIdentity,
        via_control: bool,
    ) -> Result<Self, TokenStoreError> {
        Ok(Self {
            endpoint,
            device,
            via_control,
            tokens: store.load()?,
            username: store.username()?,
            refresh_failures: 0,
        })
    }

    pub fn is_signed_in(&self) -> bool {
        self.tokens.is_some()
    }

    /// Drops the tokens from this session only; the store keeps them. Stock does this when
    /// the pair after sign-in fails (VAs 0x0030afb8, 0x0044d590).
    pub fn forget_tokens(&mut self) {
        self.tokens = None;
        self.refresh_failures = 0;
    }

    /// The signed-in account name, as last loaded or signed in.
    pub fn username(&self) -> Option<&str> {
        self.username.as_deref()
    }

    pub fn claims(&self) -> Option<Claims> {
        self.tokens
            .as_ref()
            .and_then(|t| Claims::parse(&t.access()))
    }

    /// A request with the identity headers, JSON content type and, when `auth`, the bearer token.
    pub fn request(&self, method: Method, url: String, auth: bool, route: Route) -> HttpRequest {
        let (type_header, version_header) = if self.via_control {
            (
                "X-Cloud-Api-Hardware-Source-Type",
                "X-Cloud-Api-Hardware-Source-Version",
            )
        } else {
            ("X-Cloud-Api-Client-Type", "X-Cloud-Api-Client-Version")
        };
        let mut request = HttpRequest::new(method, url, route, DEFAULT_TIMEOUT)
            .header(type_header, self.device.client_type.as_str())
            .header(version_header, self.device.firmware.clone())
            .header("Content-Type", "application/json");
        if let Some(tokens) = self.tokens.as_ref().filter(|_| auth) {
            request = request.header("Authorization", format!("Bearer {}", tokens.access()));
        }
        request
    }

    /// An authenticated call without a body; refreshes the token first when it is near
    /// expiry. Calls with a body use [`Self::ensure_fresh`], [`Self::request`] and [`exchange`].
    pub fn call(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
        method: Method,
        url: String,
        route: Route,
        now_secs: i64,
    ) -> Result<Value, ApiError> {
        self.ensure_fresh(http, store, now_secs)?;
        exchange(http, &self.request(method, url, true, route))
    }

    fn save_tokens(
        &mut self,
        store: &mut impl TokenStore,
        tokens: Tokens,
        username: Option<&str>,
    ) -> Result<(), TokenStoreError> {
        store.store(&tokens, username)?;
        if let Some(name) = username {
            self.username = Some(name.to_owned());
        }
        self.tokens = Some(tokens);
        self.refresh_failures = 0;
        Ok(())
    }

    /// Local logout with no request (stock `0x002ff6e0` → `0x0044d590`): the store and the
    /// in-memory session forget the user and the tokens.
    pub fn clear_tokens(&mut self, store: &mut impl TokenStore) -> Result<(), TokenStoreError> {
        store.clear()?;
        self.tokens = None;
        self.username = None;
        self.refresh_failures = 0;
        Ok(())
    }

    fn take_tokens(reply: Value) -> Result<Tokens, ApiError> {
        #[derive(serde::Deserialize)]
        struct Reply {
            access_token: String,
            refresh_token: String,
        }
        let reply: Reply =
            serde_json::from_value(reply).map_err(|e| ApiError::Reply(e.to_string()))?;
        Claims::parse(&reply.access_token)
            .ok_or_else(|| ApiError::Reply("access token has no user id or expiry".into()))?;
        Ok(Tokens {
            access_token: Secret::new(reply.access_token),
            refresh_token: Secret::new(reply.refresh_token),
        })
    }

    /// `POST user/signin {"username","password"}`, no bearer. Saves the tokens and username.
    pub fn sign_in(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
        username: &str,
        password: &str,
    ) -> Result<(), ApiError> {
        let request = self
            .request(
                Method::Post,
                self.endpoint.api("user/signin"),
                false,
                Route::Product,
            )
            .body(json!({ "username": username, "password": password }).to_string());
        let tokens = Self::take_tokens(exchange(http, &request)?)?;
        Ok(self.save_tokens(store, tokens, Some(username))?)
    }

    /// `loginFromControl` `0x307e18` → `0x450158`: the tokens Cortex Control signed in with.
    /// An access token without a user id and an expiry is refused ("Got invalid access token
    /// from Control") and the session forgets its tokens (`0x44d590`); else the tokens and
    /// the username are saved. Inferred: the claim check stands for the stock test `0x44ed80`.
    pub fn adopt_tokens(
        &mut self,
        store: &mut impl TokenStore,
        tokens: Tokens,
        username: Option<&str>,
    ) -> Result<(), ApiError> {
        if Claims::parse(&tokens.access()).is_none() {
            self.forget_tokens();
            return Err(ApiError::Reply("invalid access token from Control".into()));
        }
        Ok(self.save_tokens(store, tokens, username)?)
    }

    /// `POST user/signout`, then forget the tokens whatever the reply.
    pub fn sign_out(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
    ) -> Result<(), ApiError> {
        let result = match self.tokens.is_some() {
            true => {
                let request = self.request(
                    Method::Post,
                    self.endpoint.api("user/signout"),
                    true,
                    Route::Product,
                );
                exchange(http, &request).map(drop)
            }
            false => Ok(()),
        };
        self.clear_tokens(store)?;
        result
    }

    /// `POST user/refresh_token {"access_token","refresh_token"}`. After more than five failures
    /// in a row the tokens are cleared (stock `tokensCleared`).
    pub fn refresh(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
    ) -> Result<(), ApiError> {
        let tokens = self.tokens.as_ref().ok_or(ApiError::NotSignedIn)?;
        let request = self
            .request(
                Method::Post,
                self.endpoint.api("user/refresh_token"),
                false,
                Route::Product,
            )
            .body(
                json!({
                    "access_token": tokens.access(),
                    "refresh_token": String::from_utf8_lossy(tokens.refresh_token.expose()),
                })
                .to_string(),
            );
        match exchange(http, &request).and_then(Self::take_tokens) {
            Ok(tokens) => Ok(self.save_tokens(store, tokens, None)?),
            Err(error) => {
                self.refresh_failures += 1;
                if self.refresh_failures > MAX_REFRESH_FAILURES {
                    self.clear_tokens(store)?;
                    return Err(ApiError::SignedOut);
                }
                Err(error)
            }
        }
    }

    /// Refresh when the access token is within its refresh lead of expiry, or unreadable.
    pub fn ensure_fresh(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
        now_secs: i64,
    ) -> Result<(), ApiError> {
        let tokens = self.tokens.as_ref().ok_or(ApiError::NotSignedIn)?;
        match Claims::parse(&tokens.access()) {
            Some(claims) if !claims.needs_refresh(now_secs) => Ok(()),
            _ => self.refresh(http, store),
        }
    }

    /// `GET user/profile`.
    pub fn profile(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
        now_secs: i64,
    ) -> Result<Value, ApiError> {
        let url = self.endpoint.api("user/profile");
        self.call(http, store, Method::Get, url, Route::Product, now_secs)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::cloud::endpoint::{CloudDomain, CloudStage};
    use crate::cloud::session::tests::jwt;
    use crate::cloud::{ClientType, DeviceIdentity};
    use crate::fake::{MemTokens, ScriptedHttp};

    /// A signed-out session with its backends, as the platform holds them.
    pub(crate) fn api() -> (CloudApi, ScriptedHttp, MemTokens) {
        let mut store = MemTokens::default();
        let api = CloudApi::load(
            &mut store,
            CloudEndpoint {
                stage: CloudStage::Production,
                domain: CloudDomain::Default,
            },
            DeviceIdentity {
                client_type: ClientType::QuadCortex,
                firmware: "4.1.0".into(),
                serial: "Q0000TEST".into(),
            },
            false,
        )
        .unwrap();
        (api, ScriptedHttp::default(), store)
    }

    fn token_reply(exp: i64) -> String {
        json!({
            "access_token": jwt(&format!(r#"{{"sub":"u-1","exp":{exp},"iat":{}}}"#, exp - 3600)),
            "refresh_token": "refresh-1",
        })
        .to_string()
    }

    #[test]
    fn sign_in_stores_tokens_and_refreshes_before_expiry() {
        let (mut api, mut http, mut store) = api();
        http.respond(200, token_reply(10_000));
        api.sign_in(&mut http, &mut store, "player@example.test", "pw")
            .unwrap();
        let sent = &http.requests[0];
        assert_eq!(
            sent.url,
            "https://cloud-api.neuraldsp.com/api/v1/user/signin"
        );
        assert_eq!(sent.header_value("Authorization"), None);
        assert_eq!(
            sent.header_value("X-Cloud-Api-Client-Type"),
            Some("quad_cortex")
        );
        assert!(store.tokens.is_some());
        assert_eq!(store.username.as_deref(), Some("player@example.test"));

        // 1 h token: refresh lead is 30 min, so 8200 s is inside it.
        http.respond(200, token_reply(20_000));
        http.respond(200, r#"{"username":"player"}"#);
        assert_eq!(
            api.profile(&mut http, &mut store, 8_200).unwrap()["username"],
            "player"
        );
        assert!(http.requests[1].url.ends_with("/user/refresh_token"));
        let bearer = http.requests[2].header_value("Authorization").unwrap();
        assert!(bearer.starts_with("Bearer e30."));
        // A refresh keeps the saved username.
        assert_eq!(store.username.as_deref(), Some("player@example.test"));
    }

    #[test]
    fn errors_map_to_stock_text_and_repeated_refresh_failure_signs_out() {
        let (mut api, mut http, mut store) = api();
        http.respond(401, r#"{"error":3202,"message":"bad","requestId":"r-1"}"#);
        let Err(ApiError::Cloud(error)) = api.sign_in(&mut http, &mut store, "a", "b") else {
            panic!("expected cloud error");
        };
        assert_eq!(error.request_id.as_deref(), Some("r-1"));
        assert!(
            error
                .text()
                .starts_with("Incorrect email address or password.")
        );
        assert_eq!(CloudError::from_reply(500, b"oops").code, -1);

        http.respond(200, token_reply(10_000));
        api.sign_in(&mut http, &mut store, "a", "b").unwrap();
        for _ in 0..MAX_REFRESH_FAILURES {
            http.respond(500, "{}");
            assert!(matches!(
                api.refresh(&mut http, &mut store),
                Err(ApiError::Cloud(_))
            ));
        }
        http.respond(500, "{}");
        assert_eq!(api.refresh(&mut http, &mut store), Err(ApiError::SignedOut));
        assert!(!api.is_signed_in());
        assert!(store.tokens.is_none() && store.username.is_none());
        assert_eq!(
            api.ensure_fresh(&mut http, &mut store, 0),
            Err(ApiError::NotSignedIn)
        );
    }
}
