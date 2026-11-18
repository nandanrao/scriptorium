//! What the apps server does with a request, decided from its head alone.
//!
//! [`decide`] runs the checks in order (header size, `Host`, `Origin`,
//! the cookie) and then the route table. It reads no files: a
//! [`Decision`] names what the shell should read, write or call.
//!
//! Routes (`<key>` is the project key, `<name>` the app):
//!
//! | Request | Decision |
//! |---|---|
//! | `GET /open?t=<token>&to=/p/<key>/<name>/` | [`Decision::Exchange`] (no cookie needed) |
//! | `GET /` | [`Decision::Index`] |
//! | `GET /scriptorium.js` | [`Decision::Library`] |
//! | `GET /p/<key>/<name>` | [`Decision::Redirect`] to the same with `/` |
//! | `GET /p/<key>/<name>/<path>` | [`Decision::File`], or [`Decision::NotOpenPage`] |
//! | `GET /api/status` | [`Decision::Status`] |
//! | `GET /api/events` | [`Decision::Events`] |
//! | `POST /api/p/<key>/a/<name>/call` | [`Decision::Call`] |
//! | `POST /api/p/<key>/a/<name>/log` | [`Decision::Log`] |
//! | `GET /api/p/<key>/a/<name>/data/` | [`Decision::DataList`] |
//! | `GET`/`PUT`/`DELETE /api/p/<key>/a/<name>/data/<path>` | [`Decision::DataRead`]/`Write`/`Remove` |
//!
//! Every per-app API route names the project in its path, as pages do, so
//! a page left open on one folder never acts on another folder's script or
//! data: a `<key>` that isn't the open folder's is answered `409
//! {"error": "not-open"}`.

use crate::list::Found;
use crate::manifest::name_ok;
use crate::paths::{self, Part, decode_and_split, ends_in_slash, key_ok, percent_decode};
use crate::{BODY_MAX, DATA_MAX, HEADERS_MAX};

/// A request's head, as the server read it.
#[derive(Debug, Clone, Default)]
pub struct Head {
    /// `GET`, `POST`, … (compared exactly: HTTP methods are case-sensitive).
    pub method: String,
    /// The raw path, without the query.
    pub path: String,
    /// The raw query, without the `?` (empty when there is none).
    pub query: String,
    /// Every header, names as sent (compared ignoring case).
    pub headers: Vec<(String, String)>,
    /// The bytes the request line and headers took.
    pub header_bytes: usize,
}

impl Head {
    /// Every value of header `name`, in order.
    pub fn header_values<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.headers.iter().filter(move |(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str().trim())
    }

    /// Header `name` when it is sent exactly once.
    fn single<'a>(&'a self, name: &'a str) -> Option<&'a str> {
        let mut values = self.header_values(name);
        let first = values.next()?;
        values.next().is_none().then_some(first)
    }
}

/// What the server knows when it decides.
#[derive(Debug, Clone)]
pub struct State {
    /// The port it listens on (`Host` and `Origin` must name it).
    pub port: u16,
    /// The per-install token (the cookie's value). Never empty.
    pub token: String,
    /// The open folder's project key, if a script is open.
    pub project: Option<String>,
}

/// What to do with a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Answer with this status and page; nothing else happens.
    Refuse(u16, Page),
    /// Set the cookie ([`cookie_line`]) and answer [`exchange_page`].
    Exchange {
        to: String,
    },
    /// 302 to `to` (an app's address without its last `/`).
    Redirect {
        to: String,
    },
    /// The open folder's apps, as links ([`index_page`]).
    Index,
    /// [`crate::LIBRARY`].
    Library,
    /// A file from `apps/<app>/`: `path` is under it, never under `data/`.
    File {
        app: String,
        path: Vec<Part>,
    },
    /// The page asked for belongs to a folder that isn't open:
    /// [`NOT_OPEN_PAGE`], status 409.
    NotOpenPage,
    /// `{"app": "scriptorium", "version", "project", "script"}`.
    Status,
    /// Run the call in the body for `app`, on project `project`.
    Call {
        app: String,
        project: String,
    },
    /// Keep what the page reports (`{"loaded": true}` or `{"error"}`).
    Log {
        app: String,
    },
    /// `[{"path", "size"}]` of `apps/<app>/data/`.
    DataList {
        app: String,
    },
    DataRead {
        app: String,
        path: Vec<Part>,
    },
    DataWrite {
        app: String,
        path: Vec<Part>,
    },
    DataRemove {
        app: String,
        path: Vec<Part>,
    },
    /// The event stream.
    Events,
}

impl Decision {
    /// The largest body this decision accepts; `None`: a body is not read.
    /// The server enforces it while reading too (a body without
    /// `Content-Length`, or one longer than it said).
    pub fn body_max(&self) -> Option<u64> {
        match self {
            Decision::Call { .. } | Decision::Log { .. } => Some(BODY_MAX),
            Decision::DataWrite { .. } => Some(DATA_MAX),
            _ => None,
        }
    }
}

/// A refusal's page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    /// 401: open the app from the Apps menu ([`UNAUTHORIZED_PAGE`]).
    Unauthorized,
    BadRequest,
    Forbidden,
    NotFound,
    /// 409 `{"error": "not-open"}`: an API call for a folder that isn't open.
    NotOpen,
    TooLarge,
    /// 421: a `Host` that isn't this server.
    Misdirected,
    HeadersTooLarge,
}

impl Page {
    /// The content type and body to send.
    pub fn body(self) -> (&'static str, String) {
        let text = |s: &str| ("text/plain; charset=utf-8", s.to_string());
        match self {
            Page::Unauthorized => ("text/html; charset=utf-8", UNAUTHORIZED_PAGE.to_string()),
            Page::NotOpen => ("application/json", r#"{"error":"not-open"}"#.to_string()),
            Page::BadRequest => text("Bad request."),
            Page::Forbidden => text("Forbidden: this request didn't come from a Scriptorium app page."),
            Page::NotFound => text("Not found."),
            Page::TooLarge => text("Too large."),
            Page::Misdirected => text("This server only answers at 127.0.0.1 or localhost."),
            Page::HeadersTooLarge => text("Request headers too large."),
        }
    }
}

/// The 401 page.
pub const UNAUTHORIZED_PAGE: &str = include_str!("../assets/unauthorized.html");
/// The page for an app whose folder isn't open; the library on it says
/// which script to open.
pub const NOT_OPEN_PAGE: &str = include_str!("../assets/not-open.html");
const EXCHANGE_PAGE: &str = include_str!("../assets/exchange.html");
const INDEX_PAGE: &str = include_str!("../assets/index.html");

/// The cookie's name.
pub const COOKIE: &str = "scriptorium";

/// Decides what to do with `head`. The checks come first, in this order:
/// header size (431), `Host` (421), `Origin` on `POST`/`PUT`/`DELETE`
/// (403), the cookie except on `GET /open` (401); then the route (404 for
/// anything not listed, including `OPTIONS` and `HEAD`).
pub fn decide(head: &Head, state: &State) -> Decision {
    if head.header_bytes > HEADERS_MAX {
        return Decision::Refuse(431, Page::HeadersTooLarge);
    }
    let ours = |authority: &str| {
        authority.eq_ignore_ascii_case(&format!("127.0.0.1:{}", state.port))
            || authority.eq_ignore_ascii_case(&format!("localhost:{}", state.port))
    };
    if !head.single("host").is_some_and(ours) {
        return Decision::Refuse(421, Page::Misdirected);
    }
    let method = head.method.as_str();
    if matches!(method, "POST" | "PUT" | "DELETE") {
        let origin_ok = head.single("origin").and_then(|o| o.strip_prefix("http://")).is_some_and(ours);
        if !origin_ok {
            return Decision::Refuse(403, Page::Forbidden);
        }
    }
    if method == "GET" && head.path == "/open" {
        return exchange(head, state);
    }
    if !has_cookie(head, &state.token) {
        return Decision::Refuse(401, Page::Unauthorized);
    }
    let decision = route(head, state);
    if let Some(max) = decision.body_max()
        && let Some(len) = head.single("content-length")
        && len.parse::<u64>().map_or(true, |n| n > max)
    {
        return Decision::Refuse(413, Page::TooLarge);
    }
    decision
}

fn route(head: &Head, state: &State) -> Decision {
    let not_found = Decision::Refuse(404, Page::NotFound);
    let Ok(parts) = decode_and_split(&head.path) else {
        return not_found;
    };
    let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
    let open = |key: &str| state.project.as_deref() == Some(key);
    match (head.method.as_str(), parts.as_slice()) {
        ("GET", []) => Decision::Index,
        ("GET", ["scriptorium.js"]) => Decision::Library,
        ("GET", ["api", "status"]) => Decision::Status,
        ("GET", ["api", "events"]) => Decision::Events,
        ("GET", ["p", key, name, rest @ ..]) if key_ok(key) && name_ok(name) => {
            if rest.is_empty() && !ends_in_slash(&head.path) {
                return Decision::Redirect { to: format!("/p/{key}/{name}/") };
            }
            let rest: Vec<Part> = rest.iter().map(|s| s.to_string()).collect();
            let Ok(path) = paths::static_path(&rest) else {
                return not_found;
            };
            if !open(key) {
                return Decision::NotOpenPage;
            }
            Decision::File { app: name.to_string(), path }
        }
        (method, ["api", "p", key, "a", name, rest @ ..]) if key_ok(key) && name_ok(name) => {
            let app = name.to_string();
            let api = match (method, rest) {
                ("POST", ["call"]) => Decision::Call { app, project: key.to_string() },
                ("POST", ["log"]) => Decision::Log { app },
                ("GET", ["data"]) => Decision::DataList { app },
                (_, ["data", path @ ..]) if !path.is_empty() => {
                    let path = paths::data_path(&path.iter().map(|s| s.to_string()).collect::<Vec<_>>());
                    match method {
                        "GET" => Decision::DataRead { app, path },
                        "PUT" => Decision::DataWrite { app, path },
                        "DELETE" => Decision::DataRemove { app, path },
                        _ => return not_found,
                    }
                }
                _ => return not_found,
            };
            if open(key) { api } else { Decision::Refuse(409, Page::NotOpen) }
        }
        _ => not_found,
    }
}

/// `GET /open?t=<token>&to=/p/<key>/<name>/`: a right token and a `to`
/// naming an app's address. `to` is rebuilt from its checked parts, so
/// nothing from the query reaches the page unchecked.
fn exchange(head: &Head, state: &State) -> Decision {
    let (mut token, mut to) = (None, None);
    for pair in head.query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let Ok(v) = percent_decode(v) else {
            return Decision::Refuse(400, Page::BadRequest);
        };
        match k {
            "t" => token = Some(v),
            "to" => to = Some(v),
            _ => {}
        }
    }
    if !token.is_some_and(|t| same(&t, &state.token)) {
        return Decision::Refuse(401, Page::Unauthorized);
    }
    let to = to.unwrap_or_default();
    match decode_and_split(&to).as_deref() {
        Ok([p, key, name]) if p == "p" && key_ok(key) && name_ok(name) && to.ends_with('/') => {
            Decision::Exchange { to: format!("/p/{key}/{name}/") }
        }
        _ => Decision::Refuse(400, Page::BadRequest),
    }
}

/// Whether any `Cookie` header carries `scriptorium=<token>`.
fn has_cookie(head: &Head, token: &str) -> bool {
    head.header_values("cookie")
        .flat_map(|line| line.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .any(|(name, value)| name == COOKIE && same(value, token))
}

/// Compares without stopping at the first difference. An empty token
/// never matches.
fn same(a: &str, b: &str) -> bool {
    if a.is_empty() || a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The `Set-Cookie` value for the exchange: lasts a year, across restarts.
pub fn cookie_line(token: &str) -> String {
    format!("{COOKIE}={token}; HttpOnly; SameSite=Lax; Path=/; Max-Age=31536000")
}

/// The content security policy: nothing loads from anywhere but this
/// server; inline scripts and styles are allowed (agents write them).
pub const CSP: &str = "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; \
                       img-src 'self' data: blob:; connect-src 'self'; frame-ancestors 'none'";

/// The headers every answer carries, refusals included.
pub fn headers() -> [(&'static str, &'static str); 4] {
    [
        ("Cache-Control", "no-store"),
        ("X-Content-Type-Options", "nosniff"),
        ("Referrer-Policy", "no-referrer"),
        ("Content-Security-Policy", CSP),
    ]
}

/// The content type for a file extension (ignoring case); unknown ones are
/// `application/octet-stream`.
pub fn mime(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "txt" | "md" => "text/plain; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// The page `/open` answers: it moves on to `to` (already checked by
/// [`decide`]), so the token leaves the address bar.
pub fn exchange_page(to: &str) -> String {
    EXCHANGE_PAGE.replace("{{to}}", &escape(to))
}

/// `GET /`: the open folder's apps as links (`project` is its key), or a
/// line saying no script is open.
pub fn index_page(project: Option<&str>, apps: &[Found]) -> String {
    let body = match project {
        None => "<p>No script is open in Scriptorium. Open one to use its apps.</p>".to_string(),
        Some(_) if apps.is_empty() => {
            "<p>The open script's folder has no apps. An agent can build one: see <code>guide('app')</code>.</p>"
                .to_string()
        }
        Some(key) => {
            let items: String = apps
                .iter()
                .map(|app| match (&app.manifest, app.problems.first()) {
                    (Some(m), None) => format!(
                        "<li><a href=\"/p/{key}/{name}/\">{title}</a> <span>{desc}</span></li>\n",
                        name = escape(&app.name),
                        title = escape(&m.title),
                        desc = escape(&m.description)
                    ),
                    (_, problem) => format!(
                        "<li>{name} <span>can't open: {why}</span></li>\n",
                        name = escape(&app.name),
                        why = escape(&problem.map(|p| p.to_string()).unwrap_or_default())
                    ),
                })
                .collect();
            format!("<ul>\n{items}</ul>")
        }
    };
    INDEX_PAGE.replace("{{body}}", &body)
}

/// Escapes text for HTML (body and quoted attributes).
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}
