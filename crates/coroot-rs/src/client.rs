//! The HTTP client: connection settings, credentials, users and projects.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, COOKIE, HeaderMap};
use reqwest::{Method, RequestBuilder, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use crate::error::{Error, ErrorKind, Result};
use crate::mcp::McpSession;
use crate::project::Project;
use crate::time::TimeRange;
use crate::util::{encode_segment, is_html, normalize_base_url};

const SESSION_COOKIE: &str = "coroot_session";
const USER_AGENT: &str = concat!("coroot-rs/", env!("CARGO_PKG_VERSION"));

/// How requests are authenticated.
#[derive(Clone, Default, PartialEq, Eq)]
pub enum Credentials {
    /// Anonymous access (only works when Coroot allows it).
    #[default]
    None,
    /// A user API key (`crt_...`), created in the Coroot UI under the user menu → API keys.
    /// Required for the MCP-backed operations.
    ApiKey(String),
    /// The `coroot_session` cookie from an email/password login.
    Session(String),
}

impl Credentials {
    /// Whether these are API key credentials, which the MCP-backed calls require.
    pub fn is_api_key(&self) -> bool {
        matches!(self, Credentials::ApiKey(_))
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Credentials::None => "None",
            Credentials::ApiKey(_) => "ApiKey(<redacted>)",
            Credentials::Session(_) => "Session(<redacted>)",
        })
    }
}

/// A JSON payload as Coroot returned it, before any normalization.
#[derive(Debug)]
pub struct Payload<'a> {
    /// The request path relative to the base URL (`api/project/<id>/alerts`), or
    /// `mcp:<tool>` for MCP tool results.
    pub endpoint: &'a str,
    /// The JSON as Coroot sent it (an MCP tool's result for `mcp:` endpoints).
    pub body: &'a Value,
}

type Observer = Arc<dyn Fn(&Payload) + Send + Sync>;

/// Configures a [`Client`].
#[must_use]
pub struct ClientBuilder {
    url: String,
    credentials: Credentials,
    timeout: Duration,
    connect_timeout: Duration,
    accept_invalid_certs: bool,
    user_agent: String,
    http: Option<reqwest::Client>,
    observer: Option<Observer>,
    max_response_bytes: Option<u64>,
}

impl ClientBuilder {
    /// Sets the credentials; see [`Credentials`].
    pub fn credentials(mut self, c: Credentials) -> Self {
        self.credentials = c;
        self
    }

    /// Authenticates with a user API key (`crt_...`). Required for MCP-backed calls.
    pub fn api_key(self, key: impl Into<String>) -> Self {
        self.credentials(Credentials::ApiKey(key.into()))
    }

    /// Authenticates with a `coroot_session` cookie value, e.g. one saved from
    /// [`ClientBuilder::login`].
    pub fn session(self, cookie: impl Into<String>) -> Self {
        self.credentials(Credentials::Session(cookie.into()))
    }

    /// Total time allowed per request. Default: 120 seconds.
    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = d;
        self
    }

    /// Default: 10 seconds.
    pub fn connect_timeout(mut self, d: Duration) -> Self {
        self.connect_timeout = d;
        self
    }

    /// Skip TLS certificate verification.
    pub fn accept_invalid_certs(mut self, yes: bool) -> Self {
        self.accept_invalid_certs = yes;
        self
    }

    /// Sets the `User-Agent` header (default: `coroot-rs/<version>`).
    pub fn user_agent(mut self, ua: impl Into<String>) -> Self {
        self.user_agent = ua.into();
        self
    }

    /// Use a preconfigured HTTP client (proxies, custom roots, ...). The timeout, TLS and
    /// user-agent settings of this builder are then ignored.
    pub fn http_client(mut self, http: reqwest::Client) -> Self {
        self.http = Some(http);
        self
    }

    /// Fails any response whose body is larger than `bytes` with
    /// [`ErrorKind::ResponseTooLarge`], before anything is decoded. Applies to every body
    /// this crate reads (data, error responses, MCP) and to [`Client::read_body`], also
    /// with [`ClientBuilder::http_client`]. Default: no limit.
    ///
    /// A body is rejected without reading it when its `Content-Length` is over the limit,
    /// and otherwise as soon as the bytes read pass it. The limit counts the body bytes
    /// reqwest yields, after transfer decoding. The client this builder creates does not
    /// request compression, so those are the bytes Coroot sends; with a decompressing
    /// client from [`ClientBuilder::http_client`], they are the decompressed bytes.
    pub fn max_response_bytes(mut self, bytes: u64) -> Self {
        self.max_response_bytes = Some(bytes);
        self
    }

    /// Calls `f` with every JSON payload Coroot returns, before normalization: useful for
    /// logging, debugging, or recording test fixtures.
    pub fn on_payload(mut self, f: impl Fn(&Payload) + Send + Sync + 'static) -> Self {
        self.observer = Some(Arc::new(f));
        self
    }

    /// Builds the client. Fails with [`ErrorKind::InvalidInput`] on an invalid URL, or with
    /// [`ErrorKind::Network`] when the HTTP client cannot be created (TLS setup).
    pub fn build(self) -> Result<Client> {
        let base = normalize_base_url(&self.url)?;
        let http = match self.http {
            Some(h) => h,
            None => reqwest::Client::builder()
                .user_agent(self.user_agent)
                .timeout(self.timeout)
                .connect_timeout(self.connect_timeout)
                .danger_accept_invalid_certs(self.accept_invalid_certs)
                .build()
                .map_err(|e| {
                    Error::new(
                        ErrorKind::Network,
                        format!("cannot create the HTTP client: {e}"),
                    )
                    .with_source(e)
                })?,
        };
        Ok(Client {
            inner: Arc::new(Inner {
                http,
                base,
                credentials: self.credentials,
                observer: self.observer,
                max_response_bytes: self.max_response_bytes,
            }),
        })
    }

    /// Logs in with an email and password and returns a client that uses the session.
    /// The session cookie is available from [`Client::credentials`] to store and reuse.
    pub async fn login(self, email: &str, password: &str) -> Result<Client> {
        let anon = ClientBuilder {
            credentials: Credentials::None,
            observer: None,
            http: self.http.clone(),
            url: self.url.clone(),
            user_agent: self.user_agent.clone(),
            ..self
        };
        let client = anon.build()?;
        let rb = client
            .request(Method::POST, "api/login")?
            .json(&serde_json::json!({"email": email, "password": password}));
        let resp = client.execute(rb).await.map_err(|e| {
            if e.http_status() == Some(404) || e.message().contains("Invalid email") {
                Error::new(ErrorKind::Auth, "invalid email or password")
            } else {
                e
            }
        })?;
        let cookie = session_from_headers(resp.headers()).ok_or_else(|| {
            Error::new(
                ErrorKind::Auth,
                "login succeeded but no session cookie was returned",
            )
        })?;
        let inner = &client.inner;
        Ok(Client {
            inner: Arc::new(Inner {
                http: inner.http.clone(),
                base: inner.base.clone(),
                credentials: Credentials::Session(cookie),
                observer: self.observer,
                max_response_bytes: inner.max_response_bytes,
            }),
        })
    }
}

impl fmt::Debug for ClientBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientBuilder")
            .field("url", &self.url)
            .field("credentials", &self.credentials)
            .field("timeout", &self.timeout)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish_non_exhaustive()
    }
}

/// A connection to one Coroot instance. Cheap to clone; clones share the connection pool.
///
/// ```no_run
/// # async fn example() -> coroot_rs::Result<()> {
/// let client = coroot_rs::Client::builder("https://coroot.example.com")
///     .api_key("crt_...")
///     .build()?;
/// let project = client.find_project("production").await?;
/// for app in project.applications().await? {
///     println!("{} {}", app.status, app.id.short());
/// }
/// # Ok(()) }
/// ```
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    base: Url,
    credentials: Credentials,
    observer: Option<Observer>,
    max_response_bytes: Option<u64>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base_url", &self.inner.base.as_str())
            .field("credentials", &self.inner.credentials)
            .finish()
    }
}

/// A project the user can access.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectInfo {
    /// The project id, used in API paths.
    pub id: String,
    /// The display name.
    pub name: String,
}

/// The authenticated user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    /// Coroot's user id.
    #[serde(default)]
    pub id: i64,
    /// The login email.
    #[serde(default)]
    pub email: String,
    /// The display name.
    #[serde(default)]
    pub name: String,
    /// The role: `Admin`, `Editor`, `Viewer`, or a custom role.
    #[serde(default)]
    pub role: String,
    /// Set when Coroot runs without authentication (anonymous access).
    #[serde(default)]
    pub anonymous: bool,
    /// The projects the user can access.
    #[serde(default, deserialize_with = "crate::json::null_default")]
    pub projects: Vec<ProjectInfo>,
}

/// A response in Coroot's `{context, data}` envelope. Responses without an envelope have
/// a null `context`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Envelope {
    /// Page-level data: the project status, search index, alert and incident counters, and
    /// the time window Coroot used.
    pub context: Value,
    /// The requested data.
    pub data: Value,
}

impl Envelope {
    fn from_value(v: Value) -> Self {
        match v {
            Value::Object(mut m) if m.contains_key("context") && m.contains_key("data") => {
                Envelope {
                    context: m.remove("context").unwrap_or(Value::Null),
                    data: m.remove("data").unwrap_or(Value::Null),
                }
            }
            other => Envelope {
                context: Value::Null,
                data: other,
            },
        }
    }

    /// Fails on errors Coroot reports inside a successful response.
    pub(crate) fn check(self) -> Result<Self> {
        if let Some(e) = self
            .context
            .pointer("/status/error")
            .and_then(Value::as_str)
            && !e.is_empty()
        {
            let kind = if e.contains("not found") {
                ErrorKind::NotFound
            } else {
                ErrorKind::Server
            };
            return Err(Error::new(kind, e));
        }
        if self
            .context
            .pointer("/license/invalid")
            .and_then(Value::as_bool)
            == Some(true)
        {
            let msg = self
                .context
                .pointer("/license/message")
                .and_then(Value::as_str)
                .unwrap_or("invalid license");
            return Err(Error::new(ErrorKind::Server, msg));
        }
        Ok(self)
    }
}

impl Client {
    /// Starts configuring a client for the Coroot instance at `url`. The scheme defaults to
    /// `http`, a path prefix such as `https://example.com/coroot/` is kept, and a link copied
    /// from the UI (`.../p/<project>/...`) is cut back to the instance URL.
    pub fn builder(url: impl Into<String>) -> ClientBuilder {
        ClientBuilder {
            url: url.into(),
            credentials: Credentials::None,
            timeout: Duration::from_secs(120),
            connect_timeout: Duration::from_secs(10),
            accept_invalid_certs: false,
            user_agent: USER_AGENT.to_string(),
            http: None,
            observer: None,
            max_response_bytes: None,
        }
    }

    /// A client with default settings.
    pub fn new(url: impl Into<String>, credentials: Credentials) -> Result<Self> {
        Client::builder(url).credentials(credentials).build()
    }

    /// The normalized base URL, with a trailing slash. URLs copied from the browser
    /// (`.../p/<project>/applications`) are reduced to the instance URL.
    pub fn base_url(&self) -> &Url {
        &self.inner.base
    }

    /// The credentials this client sends.
    pub fn credentials(&self) -> &Credentials {
        &self.inner.credentials
    }

    /// The authenticated user and the projects they can access. A cheap way to check
    /// credentials.
    pub async fn user(&self) -> Result<User> {
        let v = self.get_json("api/user", &[]).await?;
        serde_json::from_value(v)
            .map_err(|e| Error::decode(format!("unexpected response from /api/user: {e}")))
    }

    /// The projects the user can access, sorted by name.
    pub async fn projects(&self) -> Result<Vec<ProjectInfo>> {
        let mut p = self.user().await?.projects;
        p.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(p)
    }

    /// A handle for a project id. Does not check that the project exists.
    pub fn project(&self, id: impl Into<String>) -> Project {
        Project::new(self.clone(), id.into(), TimeRange::default())
    }

    /// Finds a project by id or name (case-insensitive).
    pub async fn find_project(&self, id_or_name: &str) -> Result<Project> {
        let projects = self.projects().await?;
        let p = find_project(&projects, id_or_name)?;
        Ok(self.project(p.id.clone()))
    }

    /// Opens an MCP session without a project selected; see [`Project::mcp`].
    pub async fn mcp(&self) -> Result<McpSession> {
        McpSession::connect(self.clone()).await
    }

    /// An authenticated request to a path relative to the base URL (`api/...`, `mcp`).
    /// Send it with [`Client::execute`]. For endpoints this crate does not cover.
    pub fn request(&self, method: Method, path: &str) -> Result<RequestBuilder> {
        let url = self
            .inner
            .base
            .join(path.trim_start_matches('/'))
            .map_err(|e| Error::invalid(format!("invalid path '{path}': {e}")))?;
        let rb = self.inner.http.request(method, url);
        Ok(match &self.inner.credentials {
            Credentials::None => rb,
            Credentials::ApiKey(t) => rb.header(AUTHORIZATION, format!("Bearer {t}")),
            Credentials::Session(s) => rb.header(COOKIE, format!("{SESSION_COOKIE}={s}")),
        })
    }

    /// Sends a request; HTTP error statuses become typed errors. The body of a successful
    /// response is left unread: read it with [`Client::read_body`] to apply
    /// [`ClientBuilder::max_response_bytes`].
    pub async fn execute(&self, rb: RequestBuilder) -> Result<Response> {
        let resp = rb.send().await.map_err(|e| self.network_error(e))?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let body = match self.read_body(resp).await {
            Ok(b) => String::from_utf8_lossy(&b).trim().to_string(),
            Err(e) if e.kind() == ErrorKind::ResponseTooLarge => return Err(e),
            // The status alone still says what went wrong.
            Err(_) => String::new(),
        };
        Err(self.http_error(status.as_u16(), &body))
    }

    /// Reads a response body, failing with [`ErrorKind::ResponseTooLarge`] when it is
    /// larger than [`ClientBuilder::max_response_bytes`]. Every body this crate reads goes
    /// through here.
    pub async fn read_body(&self, mut resp: Response) -> Result<Vec<u8>> {
        let limit = self.inner.max_response_bytes;
        let too_large = |resp: &Response, limit| {
            Error::too_large(resp.url().path(), limit, resp.status().as_u16())
        };
        if let (Some(limit), Some(len)) = (limit, resp.content_length())
            && len > limit
        {
            return Err(too_large(&resp, limit));
        }
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| {
            Error::new(ErrorKind::Network, format!("cannot read the response: {e}")).with_source(e)
        })? {
            if let Some(limit) = limit
                && (body.len() + chunk.len()) as u64 > limit
            {
                return Err(too_large(&resp, limit));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    /// The limit set with [`ClientBuilder::max_response_bytes`].
    pub fn max_response_bytes(&self) -> Option<u64> {
        self.inner.max_response_bytes
    }

    /// GET a JSON document. `path` is relative to the base URL, e.g. `api/user`.
    pub async fn get_json(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        let rb = self
            .request(Method::GET, path)?
            .header(ACCEPT, "application/json")
            .query(query);
        let resp = self.execute(rb).await?;
        let v = parse_json(&self.read_body(resp).await?)?;
        self.observe(path, &v);
        Ok(v)
    }

    /// Sends an optional JSON body and returns the response body parsed as JSON, or
    /// `Value::Null` when it is empty or not JSON.
    pub async fn send_json(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value> {
        let mut rb = self.request(method, path)?;
        if let Some(b) = body {
            rb = rb
                .header(CONTENT_TYPE, "application/json")
                .body(serde_json::to_vec(b)?);
        }
        let resp = self.execute(rb).await?;
        let v = serde_json::from_slice(&self.read_body(resp).await?).unwrap_or(Value::Null);
        self.observe(path, &v);
        Ok(v)
    }

    pub(crate) async fn get_envelope(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<Envelope> {
        Envelope::from_value(self.get_json(path, query).await?).check()
    }

    pub(crate) fn observe(&self, endpoint: &str, body: &Value) {
        if let Some(f) = &self.inner.observer {
            f(&Payload { endpoint, body });
        }
    }

    pub(crate) fn project_path(&self, project: &str, suffix: &str) -> String {
        format!("api/project/{}/{suffix}", encode_segment(project))
    }

    fn network_error(&self, e: reqwest::Error) -> Error {
        let mut msg = format!("request to {} failed", self.inner.base);
        if e.is_timeout() {
            msg.push_str(": timed out");
        } else if e.is_connect() {
            msg.push_str(": cannot connect");
        }
        let detail = std::error::Error::source(&e)
            .map(|s| s.to_string())
            .unwrap_or_else(|| e.to_string());
        Error::new(ErrorKind::Network, format!("{msg}: {detail}")).with_source(e)
    }

    fn http_error(&self, code: u16, body: &str) -> Error {
        let (kind, message) = match code {
            401 if body == "set_admin_password" => (
                ErrorKind::Auth,
                format!(
                    "this Coroot instance has no admin password yet: open {} to set it",
                    self.inner.base
                ),
            ),
            401 => (
                ErrorKind::Auth,
                match self.inner.credentials {
                    Credentials::None => "authentication required",
                    Credentials::ApiKey(_) => "unauthorized: the API key was rejected",
                    Credentials::Session(_) => "unauthorized: the session has expired",
                }
                .to_string(),
            ),
            403 if body.is_empty() => (
                ErrorKind::Forbidden,
                "forbidden: your role does not allow this action".to_string(),
            ),
            403 => (ErrorKind::Forbidden, format!("forbidden: {body}")),
            404 if body.is_empty() || body.starts_with("404 page not found") => (
                ErrorKind::Unsupported,
                "not found (HTTP 404): this Coroot version may not support this feature"
                    .to_string(),
            ),
            404 => (ErrorKind::NotFound, body.to_string()),
            400 => (
                ErrorKind::InvalidInput,
                format!(
                    "bad request: {}",
                    if body.is_empty() {
                        "rejected by Coroot"
                    } else {
                        body
                    }
                ),
            ),
            _ if body.is_empty() => (ErrorKind::Server, format!("Coroot returned HTTP {code}")),
            _ => (ErrorKind::Server, format!("{body} (HTTP {code})")),
        };
        Error::new(kind, message).with_status(code)
    }
}

/// Finds a project by id, then by case-insensitive name.
impl ProjectInfo {
    /// Finds a project by id, or by name ignoring case. Fails with
    /// [`ErrorKind::NotFound`] listing the available projects as candidates.
    pub fn find<'a>(projects: &'a [ProjectInfo], id_or_name: &str) -> Result<&'a ProjectInfo> {
        find_project(projects, id_or_name)
    }
}

fn find_project<'a>(projects: &'a [ProjectInfo], q: &str) -> Result<&'a ProjectInfo> {
    projects
        .iter()
        .find(|p| p.id == q)
        .or_else(|| projects.iter().find(|p| p.name.eq_ignore_ascii_case(q)))
        .ok_or_else(|| {
            Error::not_found(format!("project '{q}' not found; available projects:"))
                .with_candidates(
                    projects
                        .iter()
                        .map(|p| format!("{} ({})", p.name, p.id))
                        .collect(),
                )
        })
}

fn parse_json(body: &[u8]) -> Result<Value> {
    let text = String::from_utf8_lossy(body);
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    if is_html(&text) {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "unknown API endpoint (Coroot answered with its web UI); this Coroot version may not support it",
        ));
    }
    serde_json::from_str(&text).map_err(|e| {
        let snippet: String = text.chars().take(200).collect();
        Error::decode(format!("expected JSON from Coroot, got: {snippet}")).with_source(e)
    })
}

fn session_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .find_map(|v| {
            let s = v.to_str().ok()?;
            let first = s.split(';').next()?;
            let (name, value) = first.split_once('=')?;
            (name.trim() == SESSION_COOKIE && !value.is_empty()).then(|| value.to_string())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelopes() {
        let e = Envelope::from_value(serde_json::json!({"context": {"x": 1}, "data": [1]}));
        assert_eq!(e.data, serde_json::json!([1]));
        let e = Envelope::from_value(serde_json::json!({"rules": []}));
        assert_eq!(e.data, serde_json::json!({"rules": []}));
        let bad = Envelope::from_value(
            serde_json::json!({"context": {"status": {"error": "Project not found"}}, "data": null}),
        );
        assert_eq!(bad.check().unwrap_err().kind(), ErrorKind::NotFound);
    }

    #[test]
    fn projects() {
        let ps = vec![
            ProjectInfo {
                id: "abc".into(),
                name: "prod".into(),
            },
            ProjectInfo {
                id: "def".into(),
                name: "staging".into(),
            },
        ];
        assert_eq!(find_project(&ps, "def").unwrap().name, "staging");
        assert_eq!(find_project(&ps, "PROD").unwrap().id, "abc");
        let e = find_project(&ps, "nope").unwrap_err();
        assert_eq!(e.kind(), ErrorKind::NotFound);
        assert_eq!(e.candidates().len(), 2);
    }

    #[test]
    fn redacted_debug() {
        let c = Credentials::ApiKey("crt_secret".into());
        assert!(!format!("{c:?}").contains("secret"));
    }
}
