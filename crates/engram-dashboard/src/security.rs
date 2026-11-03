//! The dashboard's request guard and response headers.
//!
//! Five checks, all in [`guard`], in this order:
//!
//! 1. **Host.** A DNS-rebinding page gets the browser to send requests to
//!    127.0.0.1 under the *attacker's* host name, which makes them
//!    same-origin to that page. Accepting only loopback names (or the
//!    explicit `--bind` address) with the right port refuses every such
//!    request, cookie or not. An IP-literal host cannot be rebound, so a
//!    `--bind 0.0.0.0` dashboard accepts any IP literal and still no name.
//! 2. **Token.** A random 32-byte token per run. `?token=` on a page request
//!    is swapped for an `HttpOnly; SameSite=Strict` cookie and a redirect
//!    that strips it from the address bar (and from history); the cookie, an
//!    `Authorization: Bearer` header or `?token=` on an API call then
//!    authorize every request. Another local user can connect to the port but
//!    cannot read the token, which was only printed to our terminal.
//! 3. **Origin + CSRF** on anything but GET/HEAD: the `Origin` must be this
//!    server's own and `X-EngramDB-CSRF` must carry the per-run value from
//!    `/api/session`. `SameSite=Strict` already keeps the cookie off
//!    cross-site requests; this is the second lock, and it is what keeps a
//!    future write endpoint (phase 4) safe against a same-site attacker.
//!
//! [`headers`] then stamps every response: a strict CSP (`'self'` only, no
//! inline script or style, so an injected `<script>` in memory text would
//! not run even if a renderer slipped), `X-Frame-Options: DENY` and
//! `frame-ancestors 'none'` (no clickjacking), `Referrer-Policy: no-referrer`
//! (the token never leaves in a `Referer`), `nosniff`, and `no-store` on
//! everything but the ETag'd static files.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::{ApiError, AppState};

/// The CSRF request header for non-GET calls.
pub const CSRF_HEADER: &str = "x-engramdb-csrf";

pub const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; \
     img-src 'self'; font-src 'self'; connect-src 'self'; object-src 'none'; \
     base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// Per-run secrets and the address they are valid for.
#[derive(Clone)]
pub struct Security {
    token: String,
    csrf: String,
    cookie_name: String,
    addr: SocketAddr,
}

impl std::fmt::Debug for Security {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the secrets, not even into a debug log.
        f.debug_struct("Security")
            .field("addr", &self.addr)
            .finish_non_exhaustive()
    }
}

fn random_hex(bytes: usize) -> anyhow::Result<String> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| anyhow::anyhow!("no OS randomness: {e}"))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

impl Security {
    pub fn new(addr: SocketAddr) -> anyhow::Result<Self> {
        Ok(Self::with_secrets(addr, random_hex(32)?, random_hex(16)?))
    }

    /// Fixed secrets, for tests.
    pub fn with_secrets(addr: SocketAddr, token: String, csrf: String) -> Self {
        Self {
            token,
            csrf,
            // Cookies are not isolated by port: two dashboards on one machine
            // would overwrite each other's cookie under a shared name.
            cookie_name: format!("engramdb_dashboard_{}", addr.port()),
            addr,
        }
    }

    pub fn csrf(&self) -> &str {
        &self.csrf
    }

    pub fn cookie_name(&self) -> &str {
        &self.cookie_name
    }

    /// The host part a browser on this machine should use.
    fn url_host(&self) -> String {
        match self.addr.ip() {
            IpAddr::V4(ip) if ip.is_unspecified() || ip.is_loopback() => "127.0.0.1".into(),
            IpAddr::V6(ip) if ip.is_unspecified() => "127.0.0.1".into(),
            IpAddr::V6(ip) => format!("[{ip}]"),
            IpAddr::V4(ip) => ip.to_string(),
        }
    }

    /// `http://HOST:PORT/?token=…`, the URL that is printed and opened.
    pub fn launch_url(&self) -> String {
        format!(
            "http://{}:{}/?token={}",
            self.url_host(),
            self.addr.port(),
            self.token
        )
    }

    /// Whether a `Host` header value names this server (see the module docs).
    pub fn host_allowed(&self, host: &str) -> bool {
        let (name, port) = match split_host(host) {
            Some(v) => v,
            None => return false,
        };
        let port_ok = match port {
            Some(p) => p == self.addr.port(),
            None => self.addr.port() == 80,
        };
        if !port_ok {
            return false;
        }
        if name.eq_ignore_ascii_case("localhost") {
            return true;
        }
        let Ok(ip) = name.parse::<IpAddr>() else {
            return false;
        };
        let bound = self.addr.ip();
        ip.is_loopback() || ip == bound || bound.is_unspecified()
    }

    fn token_matches(&self, candidate: &str) -> bool {
        constant_time_eq(candidate.as_bytes(), self.token.as_bytes())
    }

    fn cookie_token<'a>(&self, headers: &'a HeaderMap) -> Option<&'a str> {
        headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(';'))
            .filter_map(|kv| kv.trim().split_once('='))
            .find(|(k, _)| *k == self.cookie_name)
            .map(|(_, v)| v)
    }

    fn set_cookie(&self) -> HeaderValue {
        HeaderValue::from_str(&format!(
            "{}={}; HttpOnly; SameSite=Strict; Path=/",
            self.cookie_name, self.token
        ))
        .expect("cookie is ASCII")
    }
}

/// `name[:port]` or `[v6][:port]`. `None` for anything malformed.
fn split_host(host: &str) -> Option<(&str, Option<u16>)> {
    if let Some(rest) = host.strip_prefix('[') {
        let (ip, after) = rest.split_once(']')?;
        let port = match after {
            "" => None,
            p => Some(p.strip_prefix(':')?.parse().ok()?),
        };
        return Some((ip, port));
    }
    match host.rsplit_once(':') {
        // A bare IPv6 address without brackets is not a valid Host.
        Some((name, _)) if name.contains(':') => None,
        Some((name, port)) => Some((name, Some(port.parse().ok()?))),
        None => Some((host, None)),
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn is_api(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/")
}

fn refuse(path: &str, status: StatusCode, code: &'static str, message: &str) -> Response {
    if is_api(path) {
        ApiError::new(status, code, message).into_response()
    } else {
        let mut r = (status, format!("{message}\n")).into_response();
        r.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        r
    }
}

/// The request guard: Host, token, then Origin + CSRF for unsafe methods.
pub async fn guard(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let sec = &state.security;
    let path = req.uri().path().to_string();

    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(str::to_string);
    let Some(host) = host.filter(|h| sec.host_allowed(h)) else {
        return refuse(
            &path,
            StatusCode::FORBIDDEN,
            "host_not_allowed",
            "This dashboard only answers to localhost. Open the URL `engramdb dashboard` printed.",
        );
    };

    // Authentication.
    let query_token = req.uri().query().and_then(|q| {
        form_urlencoded::parse(q.as_bytes())
            .find(|(k, _)| k == "token")
            .map(|(_, v)| v.into_owned())
    });
    let bearer = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let by_cookie = sec
        .cookie_token(req.headers())
        .is_some_and(|t| sec.token_matches(t));
    let by_header = bearer.is_some_and(|t| sec.token_matches(t));
    let by_query = query_token.as_deref().is_some_and(|t| sec.token_matches(t));
    if !(by_cookie || by_header || by_query) {
        return refuse(
            &path,
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Missing or wrong dashboard token. Open the URL `engramdb dashboard` printed \
             (each run has a new token).",
        );
    }

    // First page load with `?token=`: trade it for the cookie and strip it.
    if by_query && !is_api(&path) && matches!(*req.method(), Method::GET | Method::HEAD) {
        let rest: Vec<(String, String)> = req
            .uri()
            .query()
            .map(|q| {
                form_urlencoded::parse(q.as_bytes())
                    .filter(|(k, _)| k != "token")
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect()
            })
            .unwrap_or_default();
        let mut location = path.clone();
        if !rest.is_empty() {
            location.push('?');
            location.push_str(
                &form_urlencoded::Serializer::new(String::new())
                    .extend_pairs(rest)
                    .finish(),
            );
        }
        let mut r = StatusCode::SEE_OTHER.into_response();
        let h = r.headers_mut();
        h.insert(header::SET_COOKIE, sec.set_cookie());
        if let Ok(v) = HeaderValue::from_str(&location) {
            h.insert(header::LOCATION, v);
        }
        return r;
    }

    if !matches!(*req.method(), Method::GET | Method::HEAD) {
        let origin_ok = req
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|o| o == format!("http://{host}"));
        let csrf_ok = req
            .headers()
            .get(CSRF_HEADER)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| constant_time_eq(v.as_bytes(), sec.csrf.as_bytes()));
        if !origin_ok || !csrf_ok {
            return refuse(
                &path,
                StatusCode::FORBIDDEN,
                "csrf",
                "A request that is not a GET needs this dashboard's own Origin and CSRF header.",
            );
        }
    }

    next.run(req).await
}

/// Stamp the security headers on every response, the guard's refusals
/// included.
pub async fn headers(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    let set = |h: &mut HeaderMap, k: header::HeaderName, v: &'static str| {
        h.insert(k, HeaderValue::from_static(v));
    };
    set(h, header::CONTENT_SECURITY_POLICY, CSP);
    set(h, header::X_FRAME_OPTIONS, "DENY");
    set(h, header::REFERRER_POLICY, "no-referrer");
    set(h, header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    set(
        h,
        header::HeaderName::from_static("cross-origin-opener-policy"),
        "same-origin",
    );
    set(
        h,
        header::HeaderName::from_static("cross-origin-resource-policy"),
        "same-origin",
    );
    if !h.contains_key(header::CACHE_CONTROL) {
        set(h, header::CACHE_CONTROL, "no-store");
    }
    res
}

/// Wrap `app` in the guard and then the header stamp (outermost), so even a
/// refusal carries the headers.
pub fn layer(
    app: axum::Router<Arc<AppState>>,
    state: Arc<AppState>,
) -> axum::Router<Arc<AppState>> {
    app.layer(axum::middleware::from_fn_with_state(state, guard))
        .layer(axum::middleware::from_fn(headers))
}

#[cfg(test)]
mod unit {
    use super::*;

    fn sec(addr: &str) -> Security {
        Security::with_secrets(addr.parse().unwrap(), "t".into(), "c".into())
    }

    #[test]
    fn host_allows_loopback_names_on_the_right_port_only() {
        let s = sec("127.0.0.1:7341");
        for ok in [
            "127.0.0.1:7341",
            "localhost:7341",
            "LOCALHOST:7341",
            "[::1]:7341",
            "127.0.0.2:7341",
        ] {
            assert!(s.host_allowed(ok), "{ok}");
        }
        for bad in [
            "127.0.0.1:7342",
            "localhost",
            "evil.example:7341",
            "localhost.evil.example:7341",
            "192.168.1.5:7341",
            "::1:7341",
            "[::1]",
            "127.0.0.1:abc",
            "",
        ] {
            assert!(!s.host_allowed(bad), "{bad}");
        }
    }

    #[test]
    fn host_allows_the_explicit_bind_address() {
        let s = sec("192.168.1.5:9000");
        assert!(s.host_allowed("192.168.1.5:9000"));
        assert!(s.host_allowed("localhost:9000"));
        assert!(!s.host_allowed("192.168.1.6:9000"));
        assert!(!s.host_allowed("myhost.lan:9000"), "names can be rebound");
        let any = sec("0.0.0.0:9000");
        assert!(any.host_allowed("10.0.0.7:9000"));
        assert!(!any.host_allowed("myhost.lan:9000"));
    }

    #[test]
    fn launch_url_uses_a_loopback_host_for_unspecified_binds() {
        assert!(sec("0.0.0.0:5")
            .launch_url()
            .starts_with("http://127.0.0.1:5/?token="));
        assert!(sec("[::1]:5")
            .launch_url()
            .starts_with("http://[::1]:5/?token="));
        assert!(sec("10.1.2.3:5")
            .launch_url()
            .starts_with("http://10.1.2.3:5/?token="));
    }

    #[test]
    fn debug_never_prints_secrets() {
        let s = Security::with_secrets(
            "127.0.0.1:1".parse().unwrap(),
            "SECRET".into(),
            "CSRF".into(),
        );
        let d = format!("{s:?}");
        assert!(!d.contains("SECRET") && !d.contains("CSRF"), "{d}");
    }

    #[test]
    fn generated_secrets_are_long_and_distinct() {
        let a = Security::new("127.0.0.1:1".parse().unwrap()).unwrap();
        let b = Security::new("127.0.0.1:1".parse().unwrap()).unwrap();
        assert_eq!(a.token.len(), 64);
        assert_ne!(a.token, b.token);
        assert_ne!(a.token, a.csrf);
    }
}
