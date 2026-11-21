use std::net::SocketAddr;

use askama::Template;
use axum::{
    Json,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    error::{ApiError, ApiResult},
    security::{
        ROOM_ID_LEN, TOKEN_LEN, client_ip, ensure_room_id, ip_key, is_valid_room_id, now, random_id,
    },
    state::{AppState, FileToken, RoomEvent},
};

const DEFAULT_EXPIRATION: i64 = 86_400; // 1 day
const MIN_EXPIRATION: i64 = 60;
const MAX_EXPIRATION: i64 = 2_592_000; // 30 days
const MAX_VIEWS_LIMIT: i64 = 1_000_000;
/// Files stay downloadable this long after the last allowed view.
const LAST_VIEW_GRACE: i64 = 900;
/// Lifetime of the file-access cookie delivered on unlock.
const FILE_TOKEN_TTL: i64 = 3600;
pub const FILE_COOKIE: &str = "st_access";

// ============================================================================
// Templates
// ============================================================================

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate;

#[derive(Template)]
#[template(path = "room.html")]
struct RoomTemplate {
    room_id: String,
    max_file_size: u64,
    p2p_threshold: u64,
    max_content_size: usize,
}

#[derive(Template)]
#[template(path = "protected.html")]
struct ProtectedTemplate {
    room_id: String,
    views_info: String,
}

#[derive(Template)]
#[template(path = "expired.html")]
struct ExpiredTemplate;

// ============================================================================
// Pages
// ============================================================================

fn render(template: impl Template) -> Response {
    match template.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => ApiError::internal(format!("template error: {e}")).into_response(),
    }
}

pub async fn index_page() -> Response {
    render(IndexTemplate)
}

pub async fn create_room() -> Redirect {
    Redirect::to(&format!("/room/{}", random_id(ROOM_ID_LEN)))
}

pub async fn room_page(
    Path(room_id): Path<String>,
    State(state): State<AppState>,
) -> ApiResult<Response> {
    if !is_valid_room_id(&room_id) {
        return Ok((StatusCode::NOT_FOUND, "Room introuvable").into_response());
    }

    if let Some(row) = state.db.get_room(&room_id).await? {
        if row.is_expired(now()) || row.views_exhausted() {
            return Ok((StatusCode::GONE, render(ExpiredTemplate)).into_response());
        }
        let views_info = match row.max_views {
            Some(max) => format!("{} ouverture(s) restante(s)", max - row.current_views),
            None => "Ouvertures illimitées".to_string(),
        };
        return Ok(render(ProtectedTemplate {
            room_id,
            views_info,
        }));
    }

    // The room itself is only created in memory when a WebSocket joins it.
    Ok(render(RoomTemplate {
        room_id,
        max_file_size: state.config.max_file_size,
        p2p_threshold: state.config.p2p_threshold,
        max_content_size: state.config.max_content_size,
    }))
}

// ============================================================================
// API
// ============================================================================

pub async fn room_info(
    Path(room_id): Path<String>,
    State(state): State<AppState>,
) -> ApiResult<Json<Value>> {
    ensure_room_id(&room_id)?;
    let row = state.db.get_room(&room_id).await?;
    Ok(Json(json!({
        "id": room_id,
        "is_protected": row.is_some(),
        "views_remaining": row
            .as_ref()
            .and_then(|r| r.max_views.map(|m| (m - r.current_views).max(0))),
        "is_persistent": row.as_ref().is_some_and(|r| r.persistent),
        "active": state.rooms.contains_key(&room_id),
    })))
}

#[derive(Debug, Deserialize)]
pub struct SaveRoomRequest {
    password: String,
    max_views: Option<i64>,
    persistent: Option<bool>,
    /// Duration in seconds (e.g. 14400 = 4h).
    expires_in: Option<i64>,
}

/// Protects ("seals") an open room: its text is frozen behind a password and
/// every connected client is disconnected. A protected room cannot be saved
/// again, so nobody can replace its password or reset its view counter.
pub async fn save_room(
    Path(room_id): Path<String>,
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(payload): Json<SaveRoomRequest>,
) -> ApiResult<Json<Value>> {
    ensure_room_id(&room_id)?;
    let ip = ip_key(client_ip(&headers, peer, state.config.trust_proxy));
    if !state.limiter.hit(format!("save:{ip}"), 10, 60) {
        return Err(ApiError::too_many());
    }

    if payload.password.is_empty() || payload.password.len() > 72 {
        return Err(ApiError::bad_request(
            "Le mot de passe doit faire entre 1 et 72 octets",
        ));
    }
    if let Some(max) = payload.max_views
        && !(1..=MAX_VIEWS_LIMIT).contains(&max)
    {
        return Err(ApiError::bad_request("Nombre d'ouvertures invalide"));
    }
    let expires_in = payload
        .expires_in
        .unwrap_or(DEFAULT_EXPIRATION)
        .clamp(MIN_EXPIRATION, MAX_EXPIRATION);
    let persistent = payload.persistent.unwrap_or(true);

    if !state.rooms.contains_key(&room_id) {
        return Err(ApiError::not_found("Room inactive"));
    }
    let already_protected = || ApiError::new(StatusCode::CONFLICT, "Cette room est déjà protégée");
    if state.db.get_room(&room_id).await?.is_some() {
        return Err(already_protected());
    }

    let password = payload.password;
    let cost = state.config.bcrypt_cost;
    let hash = tokio::task::spawn_blocking(move || bcrypt::hash(password, cost))
        .await?
        .map_err(ApiError::internal)?;

    let content = state
        .rooms
        .get(&room_id)
        .map(|r| r.content.clone())
        .ok_or_else(|| ApiError::not_found("Room inactive"))?;

    let inserted = state
        .db
        .insert_room(
            &room_id,
            &content,
            &hash,
            payload.max_views,
            now() + expires_in,
            persistent,
        )
        .await?;
    if !inserted {
        return Err(already_protected());
    }

    // P2P files can no longer be served once their owners are disconnected.
    for (id, is_p2p) in state.db.room_file_ids(&room_id).await? {
        if is_p2p {
            state.db.delete_file_row(&id).await?;
        }
    }

    if let Some((_, room)) = state.rooms.remove(&room_id) {
        let _ = room.tx.send(RoomEvent::Sealed);
    }

    Ok(Json(json!({ "success": true, "persistent": persistent })))
}

#[derive(Debug, Deserialize)]
pub struct UnlockRequest {
    password: String,
}

pub async fn unlock_room(
    Path(room_id): Path<String>,
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(payload): Json<UnlockRequest>,
) -> ApiResult<Response> {
    ensure_room_id(&room_id)?;

    // Every attempt counts, so parallel requests cannot bypass the limits.
    let ip = ip_key(client_ip(&headers, peer, state.config.trust_proxy));
    let allowed = state.limiter.hit(format!("unlock:{ip}:{room_id}"), 10, 900)
        && state.limiter.hit(format!("unlock:{ip}"), 50, 3600)
        && state
            .limiter
            .hit(format!("unlock-room:{room_id}"), 100, 3600);
    if !allowed {
        return Err(ApiError::too_many());
    }

    let now = now();
    let gone = || ApiError::new(StatusCode::GONE, "Cette room a expiré");
    let row = state
        .db
        .get_room(&room_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Room introuvable"))?;
    if row.is_expired(now) || row.views_exhausted() {
        return Err(gone());
    }

    let wrong_password = || ApiError::new(StatusCode::UNAUTHORIZED, "Mot de passe incorrect");
    let hash = row.password_hash.clone().ok_or_else(wrong_password)?;
    let password = payload.password;
    let valid =
        tokio::task::spawn_blocking(move || bcrypt::verify(password, &hash).unwrap_or(false))
            .await?;
    if !valid {
        return Err(wrong_password());
    }

    let mut room_expires = row.expires_at.unwrap_or(now + MAX_EXPIRATION);
    let mut views_remaining = None;
    let mut last_view = false;
    if let Some(max) = row.max_views {
        if !state.db.consume_view(&room_id, now).await? {
            return Err(gone());
        }
        let current = state
            .db
            .get_room(&room_id)
            .await?
            .map_or(max, |r| r.current_views);
        let remaining = (max - current).max(0);
        views_remaining = Some(remaining);
        if remaining == 0 {
            last_view = true;
            room_expires = room_expires.min(now + LAST_VIEW_GRACE);
            state.db.exhaust_room(&room_id, room_expires).await?;
        }
    }

    let files = state.db.list_files(&room_id, false).await?;
    let body = Json(json!({
        "success": true,
        "content": row.content,
        "views_remaining": views_remaining,
        "last_view": last_view,
        "files": files,
    }));

    if files.is_empty() {
        return Ok(body.into_response());
    }

    let token = random_id(TOKEN_LEN);
    let token_expires = (now + FILE_TOKEN_TTL).min(room_expires);
    state.file_tokens.insert(
        token.clone(),
        FileToken {
            room_id: room_id.clone(),
            expires_at: token_expires,
        },
    );
    let cookie = format!(
        "{FILE_COOKIE}={token}; Path=/api/room/{room_id}/; Max-Age={}; HttpOnly; SameSite=Strict{}",
        (token_expires - now).max(0),
        if state.config.cookie_secure {
            "; Secure"
        } else {
            ""
        }
    );
    Ok(([(header::SET_COOKIE, cookie)], body).into_response())
}
