use std::net::SocketAddr;

use axum::{
    Json,
    body::Body,
    extract::{ConnectInfo, Multipart, Path, State, multipart::MultipartError},
    http::{HeaderMap, StatusCode, header},
    response::Response,
};
use serde_json::{Value, json};
use tokio_util::io::ReaderStream;

use crate::{
    error::{ApiError, ApiResult},
    rooms::FILE_COOKIE,
    security::{
        FILE_ID_LEN, client_ip, content_disposition, ensure_room_id, get_cookies, ip_key,
        is_safe_id, now, random_id, sanitize_filename,
    },
    state::{AppState, FileInfo, RoomEvent},
    storage::TempUpload,
};

enum Access {
    /// Open room: anyone knowing its ID may read and write.
    Open,
    /// Protected room unlocked with a valid file token: read only.
    Unlocked,
}

fn has_file_token(state: &AppState, room_id: &str, headers: &HeaderMap) -> bool {
    let now = now();
    get_cookies(headers, FILE_COOKIE).iter().any(|token| {
        state
            .file_tokens
            .get(token)
            .is_some_and(|t| t.room_id == room_id && t.expires_at > now)
    })
}

async fn room_access(state: &AppState, room_id: &str, headers: &HeaderMap) -> ApiResult<Access> {
    if let Some(row) = state.db.get_room(room_id).await? {
        if row.is_expired(now()) {
            return Err(ApiError::new(StatusCode::GONE, "Cette room a expiré"));
        }
        if has_file_token(state, room_id, headers) {
            Ok(Access::Unlocked)
        } else {
            Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "Déverrouillez la room pour accéder aux fichiers",
            ))
        }
    } else if state.rooms.contains_key(room_id) {
        Ok(Access::Open)
    } else {
        Err(ApiError::not_found("Room introuvable ou inactive"))
    }
}

/// Uploads and deletions are only allowed in open, active rooms.
async fn require_open(state: &AppState, room_id: &str) -> ApiResult<()> {
    if state.db.get_room(room_id).await?.is_some() {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "Room protégée : lecture seule",
        ));
    }
    if !state.rooms.contains_key(room_id) {
        return Err(ApiError::not_found("Room inactive"));
    }
    Ok(())
}

fn multipart_error(e: MultipartError) -> ApiError {
    let status = e.status();
    if status == StatusCode::PAYLOAD_TOO_LARGE {
        ApiError::new(status, "Fichier trop volumineux")
    } else {
        ApiError::new(status, e.body_text())
    }
}

pub async fn list_files(
    Path(room_id): Path<String>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<FileInfo>>> {
    ensure_room_id(&room_id)?;
    let include_p2p = match room_access(&state, &room_id, &headers).await? {
        Access::Open => true,
        Access::Unlocked => false,
    };
    Ok(Json(state.db.list_files(&room_id, include_p2p).await?))
}

/// What a multipart upload request carries.
enum Upload {
    /// A file stored on the server.
    Stored { name: String, temp: TempUpload },
    /// A file announced for peer-to-peer transfer (content never uploaded).
    P2p { name: String, size: u64 },
}

/// Reads the multipart fields:
/// - `owner_id` (optional): WebSocket user id of the uploader;
/// - `file`: a file stored on the server, streamed to a temporary file, or
/// - `p2p_name` + `p2p_size`: a file announced for peer-to-peer transfer.
async fn read_upload(
    state: &AppState,
    multipart: &mut Multipart,
) -> ApiResult<(Upload, Option<String>)> {
    let mut owner_id = None;
    let mut stored = None;
    let mut p2p_name = None;
    let mut p2p_size = None;

    while let Some(mut field) = multipart.next_field().await.map_err(multipart_error)? {
        match field.name().unwrap_or("") {
            "owner_id" => {
                let value = field.text().await.map_err(multipart_error)?;
                owner_id = Some(value).filter(|v| is_safe_id(v));
            }
            "p2p_name" => {
                let value = field.text().await.map_err(multipart_error)?;
                p2p_name = Some(sanitize_filename(&value));
            }
            "p2p_size" => {
                let value = field.text().await.map_err(multipart_error)?;
                p2p_size = value.trim().parse::<u64>().ok();
            }
            "file" if stored.is_none() => {
                let name = sanitize_filename(field.file_name().unwrap_or("fichier"));
                let mut temp = state.storage.create_temp().await?;
                while let Some(chunk) = field.chunk().await.map_err(multipart_error)? {
                    if temp.size + chunk.len() as u64 > state.config.max_file_size {
                        return Err(ApiError::new(
                            StatusCode::PAYLOAD_TOO_LARGE,
                            "Fichier trop volumineux",
                        ));
                    }
                    temp.write(&chunk).await?;
                }
                stored = Some(Upload::Stored { name, temp });
            }
            _ => {}
        }
    }

    let upload = match (stored, p2p_name, p2p_size) {
        (Some(Upload::Stored { temp, .. }), _, _) if temp.size == 0 => {
            return Err(ApiError::bad_request("Fichier vide"));
        }
        (Some(upload), _, _) => upload,
        (None, Some(name), Some(size)) if size > 0 => Upload::P2p { name, size },
        _ => return Err(ApiError::bad_request("Aucun fichier fourni")),
    };
    Ok((upload, owner_id))
}

fn as_bytes(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// Checks quotas and records the file. Serialised by `upload_lock` so that
/// concurrent uploads cannot exceed the quotas, nor race the orphan sweeper.
async fn commit_upload(
    state: &AppState,
    room_id: &str,
    info: &FileInfo,
    temp: Option<TempUpload>,
) -> ApiResult<()> {
    let config = &state.config;
    let _guard = state.upload_lock.lock().await;
    require_open(state, room_id).await?;

    let (count, room_bytes) = state.db.room_usage(room_id).await?;
    if count >= config.max_files_per_room {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Nombre maximum de fichiers atteint pour cette room",
        ));
    }

    let Some(temp) = temp else {
        // P2P: metadata only.
        state.db.insert_file(room_id, info).await?;
        return Ok(());
    };
    if as_bytes(room_bytes) + info.size > config.room_quota {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Quota de stockage de la room atteint",
        ));
    }
    if as_bytes(state.db.total_usage().await?) + info.size > config.storage_quota {
        return Err(ApiError::new(
            StatusCode::INSUFFICIENT_STORAGE,
            "Espace de stockage du serveur plein",
        ));
    }
    state.storage.commit(temp, &info.id).await?;
    if let Err(e) = state.db.insert_file(room_id, info).await {
        state.storage.remove(&info.id).await;
        return Err(e.into());
    }
    Ok(())
}

pub async fn upload_file(
    Path(room_id): Path<String>,
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> ApiResult<Json<Value>> {
    ensure_room_id(&room_id)?;
    let ip = ip_key(client_ip(&headers, peer, &state.config));
    if !state.limiter.hit(format!("upload:{ip}"), 60, 60) {
        return Err(ApiError::too_many());
    }
    // Checked before reading the body, so refused uploads cost nothing.
    require_open(&state, &room_id).await?;

    let (upload, owner_id) = read_upload(&state, &mut multipart).await?;
    let (name, size, temp) = match upload {
        Upload::Stored { name, temp } => (name, temp.size, Some(temp)),
        Upload::P2p { name, size } => (name, size, None),
    };
    let info = FileInfo {
        id: random_id(FILE_ID_LEN),
        mime_type: mime_guess::from_path(&name)
            .first_or_octet_stream()
            .to_string(),
        name,
        size,
        is_p2p: temp.is_none(),
        owner_id,
    };
    commit_upload(&state, &room_id, &info, temp).await?;

    if let Some(room) = state.rooms.get(&room_id) {
        let _ = room.tx.send(RoomEvent::FileAdded { file: info.clone() });
    }
    Ok(Json(json!({ "file": info })))
}

pub async fn download_file(
    Path((room_id, file_id)): Path<(String, String)>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    ensure_room_id(&room_id)?;
    room_access(&state, &room_id, &headers).await?;

    let not_found = || ApiError::not_found("Fichier introuvable");
    let record = state
        .db
        .get_file(&room_id, &file_id)
        .await?
        .ok_or_else(not_found)?;
    if record.is_p2p {
        return Err(ApiError::not_found(
            "Fichier P2P : à demander à son propriétaire",
        ));
    }

    let file = match state.storage.open(&file_id).await {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(not_found()),
        Err(e) => return Err(e.into()),
    };
    let len = file.metadata().await?.len();

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, record.mime_type)
        .header(header::CONTENT_LENGTH, len)
        .header(
            header::CONTENT_DISPOSITION,
            content_disposition(&record.name),
        )
        .header(
            header::CONTENT_SECURITY_POLICY,
            "default-src 'none'; sandbox",
        )
        .header(header::CACHE_CONTROL, "private, no-store")
        .body(Body::from_stream(ReaderStream::new(file)))
        .map_err(ApiError::internal)
}

pub async fn delete_file(
    Path((room_id, file_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> ApiResult<Json<Value>> {
    ensure_room_id(&room_id)?;
    require_open(&state, &room_id).await?;

    let record = state
        .db
        .get_file(&room_id, &file_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Fichier introuvable"))?;
    state.db.delete_file_row(&file_id).await?;
    if !record.is_p2p {
        state.storage.remove(&file_id).await;
    }

    if let Some(room) = state.rooms.get(&room_id) {
        let _ = room.tx.send(RoomEvent::FileRemoved { file_id });
    }
    Ok(Json(json!({ "success": true })))
}

// ============================================================================
// Cleanup helpers
// ============================================================================

pub async fn purge_room_files(state: &AppState, room_id: &str) -> ApiResult<()> {
    for (id, is_p2p) in state.db.room_file_ids(room_id).await? {
        state.db.delete_file_row(&id).await?;
        if !is_p2p {
            state.storage.remove(&id).await;
        }
    }
    Ok(())
}

/// Deletes files whose room is neither protected (in DB) nor active (in memory).
pub async fn purge_orphan_files(state: &AppState) -> ApiResult<usize> {
    let _guard = state.upload_lock.lock().await;
    let mut purged = 0;
    for (id, room_id, is_p2p) in state.db.files_without_db_room().await? {
        if state.rooms.contains_key(&room_id) {
            continue;
        }
        state.db.delete_file_row(&id).await?;
        if !is_p2p {
            state.storage.remove(&id).await;
        }
        purged += 1;
    }
    Ok(purged)
}
