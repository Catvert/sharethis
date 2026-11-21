use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use dashmap::DashMap;
use rand::{RngExt, distr::Alphanumeric};

use crate::{error::ApiError, state::AppState};

/// Length of generated room IDs: 16 base62 chars ≈ 95 bits of entropy.
pub const ROOM_ID_LEN: usize = 16;
pub const FILE_ID_LEN: usize = 16;
pub const USER_ID_LEN: usize = 12;
pub const TOKEN_LEN: usize = 32;

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Random base62 identifier (ThreadRng is a CSPRNG).
pub fn random_id(len: usize) -> String {
    rand::rng()
        .sample_iter(Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

/// Room IDs: generated ones are base62, custom ones may also use `-` and `_`.
pub fn is_valid_room_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// IDs that are safe to use as a file name on disk (strictly alphanumeric).
pub fn is_safe_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

pub fn ensure_room_id(id: &str) -> Result<(), ApiError> {
    if is_valid_room_id(id) {
        Ok(())
    } else {
        Err(ApiError::not_found("Room introuvable"))
    }
}

// ============================================================================
// File names
// ============================================================================

fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Strips control / bidi characters and path separators, caps the length.
pub fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_control() && !is_bidi_control(*c))
        .map(|c| match c {
            '/' | '\\' => '_',
            _ => c,
        })
        .take(200)
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "fichier".to_string()
    } else {
        cleaned.to_string()
    }
}

/// RFC 6266 / RFC 5987 `Content-Disposition` value. Always valid ASCII.
pub fn content_disposition(name: &str) -> String {
    let fallback: String = name
        .chars()
        .map(|c| {
            if (c.is_ascii_graphic() || c == ' ') && c != '"' && c != '\\' && c != '%' {
                c
            } else {
                '_'
            }
        })
        .collect();

    let mut encoded = String::with_capacity(name.len() * 3);
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&b) {
            encoded.push(b as char);
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

// ============================================================================
// Client identification
// ============================================================================

/// Client IP. `X-Forwarded-For` is only honoured when `trust_proxy` is set; the
/// last entry is used since it is the one appended by our own reverse proxy.
pub fn client_ip(headers: &HeaderMap, peer: SocketAddr, trust_proxy: bool) -> IpAddr {
    if trust_proxy
        && let Some(ip) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit(',').next())
            .and_then(|v| v.trim().parse::<IpAddr>().ok())
    {
        return ip;
    }
    peer.ip()
}

/// Rate-limit key for an IP: IPv6 addresses are grouped by /64.
pub fn ip_key(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let s = v6.segments();
                format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
            }
        },
    }
}

/// Values of every cookie called `name`.
pub fn get_cookies(headers: &HeaderMap, name: &str) -> Vec<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|part| part.trim().split_once('='))
        .filter(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
        .collect()
}

/// Rejects cross-site WebSocket connections (browsers always send `Origin`).
pub fn origin_allowed(headers: &HeaderMap, trust_proxy: bool) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let host = trust_proxy
        .then(|| headers.get("x-forwarded-host"))
        .flatten()
        .or_else(|| headers.get(header::HOST))
        .and_then(|v| v.to_str().ok());
    let Some(host) = host else {
        return false;
    };
    let origin_host = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))
        .unwrap_or("");
    origin_host.eq_ignore_ascii_case(host)
}

// ============================================================================
// Rate limiting (fixed window, in memory)
// ============================================================================

struct Window {
    start: i64,
    len: i64,
    count: u32,
}

#[derive(Clone, Default)]
pub struct RateLimiter {
    windows: Arc<DashMap<String, Window>>,
}

impl RateLimiter {
    /// Records a hit for `key`; returns `false` once `limit` hits were made
    /// within the current window of `window_secs`.
    pub fn hit(&self, key: String, limit: u32, window_secs: i64) -> bool {
        let now = now();
        let mut w = self.windows.entry(key).or_insert(Window {
            start: now,
            len: window_secs,
            count: 0,
        });
        if now - w.start >= w.len {
            w.start = now;
            w.count = 0;
        }
        w.count = w.count.saturating_add(1);
        w.count <= limit
    }

    pub fn purge(&self) {
        let now = now();
        self.windows.retain(|_, w| now - w.start < w.len);
    }
}

/// Global per-IP request limit.
pub async fn rate_limit_middleware(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let ip = client_ip(req.headers(), peer, state.config.trust_proxy);
    if !state.limiter.hit(format!("req:{}", ip_key(ip)), 600, 60) {
        return ApiError::too_many().into_response();
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_sanitizing() {
        assert_eq!(sanitize_filename("../../etc/passwd"), ".._.._etc_passwd");
        assert_eq!(sanitize_filename("a\r\nb\"c.txt"), "ab\"c.txt");
        assert_eq!(sanitize_filename("evil\u{202E}gnp.exe"), "evilgnp.exe");
        assert_eq!(sanitize_filename("   "), "fichier");
    }

    #[test]
    fn content_disposition_is_ascii() {
        let v = content_disposition("é\"x\\y%.txt");
        assert!(v.is_ascii());
        assert!(v.contains("filename=\"__x_y_.txt\""));
        assert!(v.contains("filename*=UTF-8''%C3%A9%22x%5Cy%25.txt"));
    }

    #[test]
    fn room_ids() {
        assert!(is_valid_room_id("a1B2-c_3"));
        assert!(!is_valid_room_id("<img>"));
        assert!(!is_valid_room_id(""));
        assert!(!is_safe_id("a-b"));
        assert_eq!(random_id(ROOM_ID_LEN).len(), ROOM_ID_LEN);
    }

    #[test]
    fn rate_limiter() {
        let l = RateLimiter::default();
        assert!(l.hit("k".into(), 2, 60));
        assert!(l.hit("k".into(), 2, 60));
        assert!(!l.hit("k".into(), 2, 60));
    }
}
