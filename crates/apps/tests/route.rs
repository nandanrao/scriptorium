//! `route::decide`: each check, the exchange, and the route table.

use scriptorium_apps::route::{Decision, Head, Page, State, cookie_line, decide, exchange_page, headers, mime};

const KEY: &str = "0123456789abcdef";
const OTHER: &str = "fedcba9876543210";
const TOKEN: &str = "tok3n";

fn state() -> State {
    State { port: 28417, token: TOKEN.into(), project: Some(KEY.into()) }
}

/// A request from an app page: right `Host`, the cookie, and `Origin` on
/// writes.
fn req(method: &str, path: &str) -> Head {
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    let mut headers = vec![("Host".to_string(), "127.0.0.1:28417".to_string())];
    headers.push(("Cookie".into(), format!("other=1; scriptorium={TOKEN}")));
    if matches!(method, "POST" | "PUT" | "DELETE") {
        headers.push(("Origin".into(), "http://127.0.0.1:28417".into()));
    }
    Head { method: method.into(), path: path.into(), query: query.into(), headers, header_bytes: 300 }
}

fn with(mut head: Head, name: &str, value: Option<&str>) -> Head {
    head.headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
    if let Some(v) = value {
        head.headers.push((name.into(), v.into()));
    }
    head
}

fn status(d: &Decision) -> Option<u16> {
    match d {
        Decision::Refuse(s, _) => Some(*s),
        _ => None,
    }
}

fn file(path: &[&str]) -> Decision {
    Decision::File { app: "scene-list".into(), path: path.iter().map(|s| s.to_string()).collect() }
}

#[test]
fn host_checks() {
    let s = state();
    assert_eq!(decide(&req("GET", "/"), &s), Decision::Index);
    assert_eq!(decide(&with(req("GET", "/"), "host", Some("localhost:28417")), &s), Decision::Index);
    assert_eq!(decide(&with(req("GET", "/"), "HOST", Some("LocalHost:28417")), &s), Decision::Index);
    for bad in ["evil.example:28417", "127.0.0.1", "localhost", "127.0.0.1:80", "attacker.localhost:28417", ""] {
        assert_eq!(status(&decide(&with(req("GET", "/"), "host", Some(bad)), &s)), Some(421), "{bad}");
    }
    // A rebinding name resolves to 127.0.0.1 but the browser still sends it.
    let rebind = with(req("GET", "/api/status"), "host", Some("7f000001.rbndr.us:28417"));
    assert_eq!(status(&decide(&rebind, &s)), Some(421));
    assert_eq!(status(&decide(&with(req("GET", "/"), "host", None), &s)), Some(421));
    let mut two = req("GET", "/");
    two.headers.push(("Host".into(), "evil:28417".into()));
    assert_eq!(status(&decide(&two, &s)), Some(421));
    // The port in use, not a fixed one.
    let other_port = State { port: 4000, ..state() };
    assert_eq!(status(&decide(&req("GET", "/"), &other_port)), Some(421));
    assert_eq!(decide(&with(req("GET", "/"), "host", Some("127.0.0.1:4000")), &other_port), Decision::Index);
}

#[test]
fn origin_on_post_put_and_delete() {
    let s = state();
    let call = format!("/api/p/{KEY}/a/scene-list/call");
    let data = format!("/api/p/{KEY}/a/scene-list/data/notes.json");
    for (method, path) in [("POST", call.as_str()), ("PUT", data.as_str()), ("DELETE", data.as_str())] {
        assert_eq!(status(&decide(&with(req(method, path), "origin", None), &s)), Some(403), "{method} without");
        for bad in ["http://evil.example", "null", "https://127.0.0.1:28417", "http://127.0.0.1:28418", ""] {
            let head = with(req(method, path), "origin", Some(bad));
            assert_eq!(status(&decide(&head, &s)), Some(403), "{method} {bad}");
        }
        let local = with(req(method, path), "origin", Some("http://localhost:28417"));
        assert_eq!(status(&decide(&local, &s)), None, "{method} localhost");
        assert_eq!(status(&decide(&req(method, path), &s)), None, "{method}");
    }
    assert_eq!(
        decide(&req("DELETE", &data), &s),
        Decision::DataRemove { app: "scene-list".into(), path: vec!["notes.json".into()] }
    );
    // GET needs no Origin.
    assert_eq!(decide(&with(req("GET", &data), "origin", None), &s).body_max(), None);
}

#[test]
fn the_cookie() {
    let s = state();
    let none = with(req("GET", "/scriptorium.js"), "cookie", None);
    assert_eq!(decide(&none, &s), Decision::Refuse(401, Page::Unauthorized));
    let wrong = with(req("GET", "/scriptorium.js"), "cookie", Some("scriptorium=nope"));
    assert_eq!(status(&decide(&wrong, &s)), Some(401));
    let prefix = with(req("GET", "/scriptorium.js"), "cookie", Some("scriptorium=tok3"));
    assert_eq!(status(&decide(&prefix, &s)), Some(401));
    let empty = with(req("GET", "/"), "cookie", Some("scriptorium="));
    assert_eq!(status(&decide(&empty, &State { token: String::new(), ..state() })), Some(401));
    let other_name = with(req("GET", "/"), "cookie", Some(&format!("xscriptorium={TOKEN}")));
    assert_eq!(status(&decide(&other_name, &s)), Some(401));
    assert_eq!(decide(&req("GET", "/scriptorium.js"), &s), Decision::Library);
    // The 401 page says what to do.
    let (kind, body) = Page::Unauthorized.body();
    assert!(kind.starts_with("text/html"));
    assert!(body.contains("Open this app from Scriptorium's Apps menu, with its script open."));
}

#[test]
fn the_exchange() {
    let s = state();
    let open = |q: &str| with(req("GET", &format!("/open?{q}")), "cookie", None);
    let to = format!("/p/{KEY}/scene-list/");
    assert_eq!(decide(&open(&format!("t={TOKEN}&to={to}")), &s), Decision::Exchange { to: to.clone() });
    // `to` percent-encoded, as a link would carry it.
    let encoded = format!("t={TOKEN}&to=%2Fp%2F{KEY}%2Fscene-list%2F");
    assert_eq!(decide(&open(&encoded), &s), Decision::Exchange { to: to.clone() });
    assert_eq!(status(&decide(&open(&format!("t=bad&to={to}")), &s)), Some(401));
    assert_eq!(status(&decide(&open(&format!("to={to}")), &s)), Some(401));
    for bad in [
        "https://evil.example/",
        "//evil.example/",
        "/",
        "/api/status",
        &format!("/p/{KEY}/scene-list"),
        &format!("/p/{KEY}/scene-list/x.html"),
        &format!("/p/{KEY}/../x/"),
        "/p/nothex/scene-list/",
        &format!("/p/{KEY}/Bad/"),
        "",
    ] {
        assert_eq!(status(&decide(&open(&format!("t={TOKEN}&to={bad}")), &s)), Some(400), "{bad}");
    }
    // Still checked: Host.
    let foreign = with(open(&format!("t={TOKEN}&to={to}")), "host", Some("evil:28417"));
    assert_eq!(status(&decide(&foreign, &s)), Some(421));
    assert_eq!(cookie_line(TOKEN), "scriptorium=tok3n; HttpOnly; SameSite=Lax; Path=/; Max-Age=31536000");
    let page = exchange_page(&to);
    assert!(page.contains(&format!("<meta http-equiv=\"refresh\" content=\"0;url={to}\">")), "{page}");
    assert!(exchange_page("\"><script>").contains("&quot;&gt;&lt;script&gt;"));
}

#[test]
fn pages_of_an_app() {
    let s = state();
    let base = format!("/p/{KEY}/scene-list");
    assert_eq!(decide(&req("GET", &format!("{base}/")), &s), file(&["index.html"]));
    assert_eq!(decide(&req("GET", &format!("{base}/vendor/lib.js")), &s), file(&["vendor", "lib.js"]));
    assert_eq!(decide(&req("GET", &base), &s), Decision::Redirect { to: format!("{base}/") });
    // Another folder's page: the "not open" page, whatever is asked for.
    assert_eq!(decide(&req("GET", &format!("/p/{OTHER}/scene-list/app.js")), &s), Decision::NotOpenPage);
    let closed = State { project: None, ..state() };
    assert_eq!(decide(&req("GET", &format!("{base}/")), &closed), Decision::NotOpenPage);
    for bad in [
        format!("{base}/../other/index.html"),
        format!("{base}/%2e%2e/other/index.html"),
        format!("{base}/a/./b"),
        format!("{base}/.git/config"),
        format!("{base}/data/notes.json"),
        format!("{base}/Data/notes.json"),
        format!("{base}/data/"),
        format!("{base}//etc/passwd"),
        format!("{base}/a\\b"),
        format!("{base}/a%5Cb"),
        format!("{base}/a%2F..%2F..%2Fx"),
        format!("/p/{KEY}/Scene/"),
        format!("/p/{KEY}/.hidden/"),
        "/p/xyz/scene-list/".to_string(),
    ] {
        assert_eq!(status(&decide(&req("GET", &bad), &s)), Some(404), "{bad}");
    }
}

#[test]
fn the_api() {
    let s = state();
    let api = format!("/api/p/{KEY}/a/scene-list");
    assert_eq!(decide(&req("GET", "/api/status"), &s), Decision::Status);
    assert_eq!(decide(&req("GET", "/api/events"), &s), Decision::Events);
    assert_eq!(
        decide(&req("POST", &format!("{api}/call")), &s),
        Decision::Call { app: "scene-list".into(), project: KEY.into() }
    );
    assert_eq!(decide(&req("POST", &format!("{api}/log")), &s), Decision::Log { app: "scene-list".into() });
    assert_eq!(decide(&req("GET", &format!("{api}/data/")), &s), Decision::DataList { app: "scene-list".into() });
    assert_eq!(decide(&req("GET", &format!("{api}/data")), &s), Decision::DataList { app: "scene-list".into() });
    let path = vec!["img".to_string(), "a b.png".to_string()];
    let app = || "scene-list".to_string();
    assert_eq!(
        decide(&req("GET", &format!("{api}/data/img/a%20b.png")), &s),
        Decision::DataRead { app: app(), path: path.clone() }
    );
    assert_eq!(
        decide(&req("PUT", &format!("{api}/data/img/a%20b.png")), &s),
        Decision::DataWrite { app: app(), path: path.clone() }
    );
    for (method, bad) in [
        ("GET", format!("{api}/call")),
        ("POST", format!("{api}/data/x")),
        ("PUT", format!("{api}/data/")),
        ("DELETE", format!("{api}/data")),
        ("GET", format!("{api}/data/../call")),
        ("GET", format!("{api}/data/%2e%2e/%2e%2e/x")),
        ("GET", format!("{api}/data/.secret")),
        ("OPTIONS", format!("{api}/call")),
        ("HEAD", "/".to_string()),
        ("GET", "/api/p/nothex/a/scene-list/data/".to_string()),
        ("GET", format!("/api/p/{KEY}/a/Bad/data/")),
        ("GET", "/api".to_string()),
        ("GET", "/favicon.ico".to_string()),
    ] {
        assert_eq!(status(&decide(&req(method, &bad), &s)), Some(404), "{method} {bad}");
    }
}

#[test]
fn another_folder_is_409_on_every_app_api() {
    let s = state();
    let closed = State { project: None, ..state() };
    let api = format!("/api/p/{OTHER}/a/scene-list");
    for (method, path) in [
        ("POST", format!("{api}/call")),
        ("POST", format!("{api}/log")),
        ("GET", format!("{api}/data/")),
        ("GET", format!("{api}/data/notes.json")),
        ("PUT", format!("{api}/data/notes.json")),
        ("DELETE", format!("{api}/data/notes.json")),
    ] {
        assert_eq!(decide(&req(method, &path), &s), Decision::Refuse(409, Page::NotOpen), "{method} {path}");
        let own = path.replace(OTHER, KEY);
        assert_eq!(decide(&req(method, &own), &closed), Decision::Refuse(409, Page::NotOpen), "{method} closed");
    }
    assert_eq!(Page::NotOpen.body(), ("application/json", r#"{"error":"not-open"}"#.to_string()));
}

#[test]
fn sizes() {
    let s = state();
    let api = format!("/api/p/{KEY}/a/scene-list");
    let mut big = req("GET", "/");
    big.header_bytes = 16 * 1024 + 1;
    assert_eq!(status(&decide(&big, &s)), Some(431));
    big.header_bytes = 16 * 1024;
    assert_eq!(decide(&big, &s), Decision::Index);
    let call = |len: &str| with(req("POST", &format!("{api}/call")), "content-length", Some(len));
    assert_eq!(status(&decide(&call("1048576"), &s)), None);
    assert_eq!(status(&decide(&call("1048577"), &s)), Some(413));
    assert_eq!(status(&decide(&call("lots"), &s)), Some(413));
    let put = |len: &str| with(req("PUT", &format!("{api}/data/a.png")), "content-length", Some(len));
    assert_eq!(status(&decide(&put("20971520"), &s)), None);
    assert_eq!(status(&decide(&put("20971521"), &s)), Some(413));
    assert_eq!(decide(&req("POST", &format!("{api}/call")), &s).body_max(), Some(1024 * 1024));
}

#[test]
fn answers_carry_the_headers() {
    let h = headers();
    let csp = h.iter().find(|(n, _)| *n == "Content-Security-Policy").unwrap().1;
    assert_eq!(
        csp,
        "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; \
         img-src 'self' data: blob:; connect-src 'self'; frame-ancestors 'none'"
    );
    assert!(h.contains(&("Cache-Control", "no-store")));
    assert!(h.contains(&("X-Content-Type-Options", "nosniff")));
    assert!(h.contains(&("Referrer-Policy", "no-referrer")));
    assert_eq!(mime("HTML"), "text/html; charset=utf-8");
    assert_eq!(mime("js"), "text/javascript; charset=utf-8");
    assert_eq!(mime("png"), "image/png");
    assert_eq!(mime("exe"), "application/octet-stream");
}

#[test]
fn the_library_and_pages_are_there() {
    let lib = scriptorium_apps::LIBRARY;
    for needle in ["window.Scriptorium", "/api/events", "securitypolicyviolation", "Scriptorium isn't running."] {
        assert!(lib.contains(needle), "{needle}");
    }
    assert!(scriptorium_apps::route::NOT_OPEN_PAGE.contains("/scriptorium.js"));
}

#[test]
fn the_index_page() {
    use scriptorium_apps::list::Found;
    use scriptorium_apps::manifest::{Manifest, Problem};
    use scriptorium_apps::route::index_page;
    let good = Found {
        name: "scene-list".into(),
        folder: "/w/apps/scene-list".into(),
        manifest: Some(Manifest { title: "Scenes <all>".into(), description: "d".into() }),
        problems: vec![],
    };
    let bad = Found {
        name: "broken".into(),
        folder: "/w/apps/broken".into(),
        manifest: None,
        problems: vec![Problem::NoIndex],
    };
    let page = index_page(Some(KEY), &[good, bad]);
    assert!(page.contains(&format!("<a href=\"/p/{KEY}/scene-list/\">Scenes &lt;all&gt;</a>")), "{page}");
    assert!(page.contains("broken <span>can't open: index.html is missing</span>"), "{page}");
    assert!(index_page(None, &[]).contains("No script is open"));
    assert!(index_page(Some(KEY), &[]).contains("no apps"));
}
