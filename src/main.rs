use askama::Template;
use axum::{
    body::Body,
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        DefaultBodyLimit, Multipart, Path, State,
    },
    http::{header, StatusCode},
    response::{Html, IntoResponse, Json, Redirect, Response},
    routing::{get, post},
    Router,
};
use dashmap::DashMap;
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, env, sync::Arc, time::{SystemTime, UNIX_EPOCH}};
use tokio::sync::broadcast;
use tokio_rusqlite::Connection;
use tower_http::cors::CorsLayer;
use uuid::Uuid;

// ============================================================================
// Config
// ============================================================================

#[derive(Clone)]
struct Config {
    max_file_size: usize,
    p2p_threshold: usize,
    port: u16,
}

impl Config {
    fn from_env() -> Self {
        Self {
            max_file_size: env::var("MAX_FILE_SIZE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10 * 1024 * 1024), // 10MB
            p2p_threshold: env::var("P2P_THRESHOLD")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(5 * 1024 * 1024), // 5MB
            port: env::var("PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3000),
        }
    }
}

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
    max_file_size: usize,
    p2p_threshold: usize,
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
// State & Models
// ============================================================================

#[derive(Clone)]
struct Room {
    content: String,
    tx: broadcast::Sender<RoomEvent>,
}

#[derive(Clone)]
struct TempRoomProtection {
    password_hash: String,
    max_views: Option<i32>,
    current_views: i32,
    expires_at: i64, // Unix timestamp (always set)
}

type TempProtections = Arc<DashMap<String, TempRoomProtection>>;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum RoomEvent {
    #[serde(rename = "content")]
    Content { content: String, from: String },
    #[serde(rename = "users")]
    Users { count: usize },
    #[serde(rename = "file_added")]
    FileAdded { file: FileInfo },
    #[serde(rename = "file_removed")]
    FileRemoved { file_id: String },
    #[serde(rename = "p2p_signal")]
    P2PSignal {
        from: String,
        to: String,
        signal: serde_json::Value,
    },
    #[serde(rename = "p2p_request")]
    P2PRequest {
        from: String,
        file_id: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FileInfo {
    id: String,
    name: String,
    size: usize,
    mime_type: String,
    is_p2p: bool,
    owner_id: Option<String>,
}

type Rooms = Arc<DashMap<String, Room>>;
type RoomUsers = Arc<DashMap<String, HashSet<String>>>;

#[derive(Clone)]
struct AppState {
    rooms: Rooms,
    room_users: RoomUsers,
    temp_protections: TempProtections,
    db: Connection,
    config: Config,
}

#[derive(Debug, Serialize, Deserialize)]
struct SaveRoomRequest {
    password: String,
    max_views: Option<i32>,
    persistent: Option<bool>,
    expires_in: Option<i64>, // Duration in seconds (e.g., 14400 = 4h, 432000 = 5d)
}

#[derive(Debug, Serialize, Deserialize)]
struct RoomInfo {
    id: String,
    is_protected: bool,
    views_remaining: Option<i32>,
    is_persistent: bool,
}

#[derive(Debug, Deserialize)]
struct UnlockRequest {
    password: String,
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();

    let config = Config::from_env();
    let db_path = env::var("DATABASE_PATH").unwrap_or_else(|_| "sharethis.db".to_string());
    let db = Connection::open(&db_path).await.unwrap();

    db.call(|conn| {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS rooms (
                id TEXT PRIMARY KEY,
                content TEXT NOT NULL,
                password_hash TEXT,
                max_views INTEGER,
                current_views INTEGER DEFAULT 0,
                expires_at INTEGER,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )",
            [],
        )?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS files (
                id TEXT PRIMARY KEY,
                room_id TEXT NOT NULL,
                name TEXT NOT NULL,
                mime_type TEXT NOT NULL,
                size INTEGER NOT NULL,
                data BLOB,
                is_p2p INTEGER DEFAULT 0,
                owner_id TEXT,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_files_room ON files(room_id)",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();

    let state = AppState {
        rooms: Arc::new(DashMap::new()),
        room_users: Arc::new(DashMap::new()),
        temp_protections: Arc::new(DashMap::new()),
        db,
        config: config.clone(),
    };

    let app = Router::new()
        .route("/", get(index_page))
        .route("/new", get(create_room))
        .route("/room/:room_id", get(room_page))
        .route("/ws/:room_id", get(websocket_handler))
        .route("/api/room/:room_id/save", post(save_room))
        .route("/api/room/:room_id/info", get(room_info))
        .route("/api/room/:room_id/unlock", post(unlock_room))
        .route("/api/room/:room_id/files", get(list_files))
        .route("/api/room/:room_id/files", post(upload_file))
        .route("/api/room/:room_id/files/:file_id", get(download_file))
        .route("/api/room/:room_id/files/:file_id", axum::routing::delete(delete_file))
        .layer(DefaultBodyLimit::max(config.max_file_size))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = format!("0.0.0.0:{}", config.port);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    tracing::info!("Server running on http://localhost:{}", config.port);
    axum::serve(listener, app).await.unwrap();
}

// ============================================================================
// Page Handlers
// ============================================================================

async fn index_page() -> impl IntoResponse {
    IndexTemplate
}

async fn create_room() -> Redirect {
    let room_id = Uuid::new_v4().to_string()[..8].to_string();
    Redirect::to(&format!("/room/{}", room_id))
}

fn current_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

async fn room_page(
    Path(room_id): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let now = current_timestamp();

    // Check for temporary protection first
    if let Some(temp_prot) = state.temp_protections.get(&room_id) {
        // Check time expiration
        if now >= temp_prot.expires_at {
            drop(temp_prot);
            state.temp_protections.remove(&room_id);
            state.rooms.remove(&room_id);
            // Delete associated files
            let room_id_files = room_id.clone();
            let _ = state.db.call(move |conn| {
                conn.execute("DELETE FROM files WHERE room_id = ?", [&room_id_files])?;
                Ok(())
            }).await;
            return Html(ExpiredTemplate.render().unwrap());
        }
        // Check view count
        if let Some(max) = temp_prot.max_views {
            if temp_prot.current_views >= max {
                return Html(ExpiredTemplate.render().unwrap());
            }
        }
        let views_info = temp_prot
            .max_views
            .map(|m| format!("{} ouvertures restantes", m - temp_prot.current_views))
            .unwrap_or_else(|| "Illimité".to_string());

        return Html(
            ProtectedTemplate {
                room_id,
                views_info,
            }
            .render()
            .unwrap(),
        );
    }

    // Check for persistent protection in DB
    let room_id_clone = room_id.clone();
    let db_room = state
        .db
        .call(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT content, password_hash, max_views, current_views, expires_at FROM rooms WHERE id = ?",
            )?;
            let result: Result<(String, Option<String>, Option<i32>, i32, Option<i64>), _> =
                stmt.query_row([&room_id_clone], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
                });
            Ok(result.ok())
        })
        .await
        .unwrap();

    // Check time expiration for persistent rooms
    if let Some((_, _, _, _, Some(expires_at))) = &db_room {
        if now >= *expires_at {
            // Delete expired room and its files
            let room_id_del = room_id.clone();
            let _ = state.db.call(move |conn| {
                conn.execute("DELETE FROM files WHERE room_id = ?", [&room_id_del])?;
                conn.execute("DELETE FROM rooms WHERE id = ?", [&room_id_del])?;
                Ok(())
            }).await;
            state.rooms.remove(&room_id);
            return Html(ExpiredTemplate.render().unwrap());
        }
    }

    if let Some((_, Some(_), max_views, current_views, _)) = &db_room {
        if let Some(max) = max_views {
            if current_views >= max {
                return Html(ExpiredTemplate.render().unwrap());
            }
        }
        let views_info = max_views
            .map(|m| format!("{} ouvertures restantes", m - current_views))
            .unwrap_or_else(|| "Illimité".to_string());

        return Html(
            ProtectedTemplate {
                room_id,
                views_info,
            }
            .render()
            .unwrap(),
        );
    }

    let initial_content = db_room.map(|(c, _, _, _, _)| c).unwrap_or_default();

    state.rooms.entry(room_id.clone()).or_insert_with(|| {
        let (tx, _) = broadcast::channel(100);
        Room {
            content: initial_content,
            tx,
        }
    });

    Html(
        RoomTemplate {
            room_id,
            max_file_size: state.config.max_file_size,
            p2p_threshold: state.config.p2p_threshold,
        }
        .render()
        .unwrap(),
    )
}

// ============================================================================
// Room API Handlers
// ============================================================================

async fn room_info(Path(room_id): Path<String>, State(state): State<AppState>) -> Json<RoomInfo> {
    let room_id_clone = room_id.clone();
    let info = state
        .db
        .call(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT password_hash, max_views, current_views FROM rooms WHERE id = ?",
            )?;
            let result: Result<(Option<String>, Option<i32>, i32), _> =
                stmt.query_row([&room_id_clone], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                });

            match result {
                Ok((password_hash, max_views, current_views)) => {
                    let views_remaining = max_views.map(|max| max - current_views);
                    Ok(RoomInfo {
                        id: room_id_clone,
                        is_protected: password_hash.is_some(),
                        views_remaining,
                        is_persistent: true,
                    })
                }
                Err(_) => Ok(RoomInfo {
                    id: room_id_clone,
                    is_protected: false,
                    views_remaining: None,
                    is_persistent: false,
                }),
            }
        })
        .await
        .unwrap();

    Json(info)
}

const DEFAULT_EXPIRATION: i64 = 86400;      // 1 day
const MAX_EXPIRATION: i64 = 2592000;        // 30 days

async fn save_room(
    Path(room_id): Path<String>,
    State(state): State<AppState>,
    Json(payload): Json<SaveRoomRequest>,
) -> impl IntoResponse {
    let password_hash = bcrypt::hash(&payload.password, 10).unwrap();
    let is_persistent = payload.persistent.unwrap_or(true);
    let expires_in = payload.expires_in
        .unwrap_or(DEFAULT_EXPIRATION)
        .min(MAX_EXPIRATION);
    let expires_at = current_timestamp() + expires_in;

    if is_persistent {
        // Persistent: save to DB
        let content = state
            .rooms
            .get(&room_id)
            .map(|r| r.content.clone())
            .unwrap_or_default();

        let max_views = payload.max_views;
        let room_id_clone = room_id.clone();
        let password_hash_clone = password_hash.clone();

        let result = state
            .db
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO rooms (id, content, password_hash, max_views, current_views, expires_at)
                     VALUES (?1, ?2, ?3, ?4, 0, ?5)
                     ON CONFLICT(id) DO UPDATE SET
                        content = ?2,
                        password_hash = ?3,
                        max_views = ?4,
                        expires_at = ?5",
                    rusqlite::params![room_id_clone, content, password_hash_clone, max_views, Some(expires_at)],
                )?;
                Ok(())
            })
            .await;

        match result {
            Ok(_) => (
                StatusCode::OK,
                Json(serde_json::json!({"success": true, "persistent": true})),
            ),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            ),
        }
    } else {
        // Temporary: store protection in memory
        state.temp_protections.insert(
            room_id.clone(),
            TempRoomProtection {
                password_hash,
                max_views: payload.max_views,
                current_views: 0,
                expires_at,
            },
        );

        (
            StatusCode::OK,
            Json(serde_json::json!({"success": true, "persistent": false})),
        )
    }
}

async fn unlock_room(
    Path(room_id): Path<String>,
    State(state): State<AppState>,
    Json(payload): Json<UnlockRequest>,
) -> impl IntoResponse {
    // Check temporary protection first
    if let Some(mut temp_prot) = state.temp_protections.get_mut(&room_id) {
        if !bcrypt::verify(&payload.password, &temp_prot.password_hash).unwrap_or(false) {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Invalid password"})),
            );
        }

        // Get content from memory
        let content = state
            .rooms
            .get(&room_id)
            .map(|r| r.content.clone())
            .unwrap_or_default();

        if let Some(max) = temp_prot.max_views {
            if temp_prot.current_views >= max {
                return (
                    StatusCode::GONE,
                    Json(serde_json::json!({"error": "Room expired", "expired": true})),
                );
            }

            temp_prot.current_views += 1;
            let new_views = temp_prot.current_views;
            let should_delete = new_views >= max;
            let views_remaining = if should_delete { 0 } else { max - new_views };

            drop(temp_prot);

            if should_delete {
                state.temp_protections.remove(&room_id);
                state.rooms.remove(&room_id);
                // Delete associated files
                let room_id_files = room_id.clone();
                let _ = state.db.call(move |conn| {
                    conn.execute("DELETE FROM files WHERE room_id = ?", [&room_id_files])?;
                    Ok(())
                }).await;
            }

            return (
                StatusCode::OK,
                Json(serde_json::json!({
                    "success": true,
                    "content": content,
                    "views_remaining": views_remaining,
                    "last_view": should_delete
                })),
            );
        } else {
            return (
                StatusCode::OK,
                Json(serde_json::json!({
                    "success": true,
                    "content": content,
                    "views_remaining": null
                })),
            );
        }
    }

    // Check persistent protection in DB
    let room_id_clone = room_id.clone();
    let result = state
        .db
        .call(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT content, password_hash, max_views, current_views FROM rooms WHERE id = ?",
            )?;
            let result: Result<(String, Option<String>, Option<i32>, i32), _> =
                stmt.query_row([&room_id_clone], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                });
            Ok(result.ok())
        })
        .await
        .unwrap();

    match result {
        Some((content, Some(hash), max_views, current_views)) => {
            if bcrypt::verify(&payload.password, &hash).unwrap_or(false) {
                if let Some(max) = max_views {
                    if current_views >= max {
                        return (
                            StatusCode::GONE,
                            Json(serde_json::json!({"error": "Room expired", "expired": true})),
                        );
                    }

                    let room_id_clone2 = room_id.clone();
                    let new_views = current_views + 1;
                    let should_delete = new_views >= max;

                    state
                        .db
                        .call(move |conn| {
                            if should_delete {
                                conn.execute("DELETE FROM files WHERE room_id = ?", [&room_id_clone2])?;
                                conn.execute("DELETE FROM rooms WHERE id = ?", [&room_id_clone2])?;
                            } else {
                                conn.execute(
                                    "UPDATE rooms SET current_views = ? WHERE id = ?",
                                    rusqlite::params![new_views, room_id_clone2],
                                )?;
                            }
                            Ok(())
                        })
                        .await
                        .unwrap();

                    if should_delete {
                        state.rooms.remove(&room_id);
                    }

                    let views_remaining = if should_delete { 0 } else { max - new_views };
                    (
                        StatusCode::OK,
                        Json(serde_json::json!({
                            "success": true,
                            "content": content,
                            "views_remaining": views_remaining,
                            "last_view": should_delete
                        })),
                    )
                } else {
                    (
                        StatusCode::OK,
                        Json(serde_json::json!({
                            "success": true,
                            "content": content,
                            "views_remaining": null
                        })),
                    )
                }
            } else {
                (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({"error": "Invalid password"})),
                )
            }
        }
        Some((content, None, _, _)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "success": true,
                "content": content
            })),
        ),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Room not found"})),
        ),
    }
}

// ============================================================================
// File Handlers
// ============================================================================

async fn list_files(
    Path(room_id): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let room_id_clone = room_id.clone();
    let files = state
        .db
        .call(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, name, mime_type, size, is_p2p, owner_id FROM files WHERE room_id = ?",
            )?;
            let files: Vec<FileInfo> = stmt
                .query_map([&room_id_clone], |row| {
                    Ok(FileInfo {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        mime_type: row.get(2)?,
                        size: row.get(3)?,
                        is_p2p: row.get::<_, i32>(4)? == 1,
                        owner_id: row.get(5)?,
                    })
                })?
                .filter_map(|r| r.ok())
                .collect();
            Ok(files)
        })
        .await
        .unwrap();

    Json(files)
}

async fn upload_file(
    Path(room_id): Path<String>,
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> impl IntoResponse {
    let mut file_name = String::new();
    let mut file_data = Vec::new();
    let mut owner_id: Option<String> = None;

    while let Some(field) = multipart.next_field().await.unwrap() {
        let name = field.name().unwrap_or("").to_string();

        if name == "file" {
            file_name = field.file_name().unwrap_or("unknown").to_string();
            file_data = field.bytes().await.unwrap().to_vec();
        } else if name == "owner_id" {
            owner_id = Some(field.text().await.unwrap_or_default());
        }
    }

    if file_data.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "No file provided"})),
        );
    }

    if file_data.len() > state.config.max_file_size {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({"error": "File too large"})),
        );
    }

    let file_id = Uuid::new_v4().to_string()[..8].to_string();
    let mime_type = mime_guess::from_path(&file_name)
        .first_or_octet_stream()
        .to_string();
    let size = file_data.len();
    let is_p2p = size > state.config.p2p_threshold;

    let file_info = FileInfo {
        id: file_id.clone(),
        name: file_name.clone(),
        size,
        mime_type: mime_type.clone(),
        is_p2p,
        owner_id: owner_id.clone(),
    };

    let room_id_clone = room_id.clone();
    let file_id_clone = file_id.clone();
    let file_name_clone = file_name.clone();
    let mime_type_clone = mime_type.clone();
    let owner_id_clone = owner_id.clone();

    // Store file (or just metadata for P2P)
    let store_data = if is_p2p { None } else { Some(file_data) };

    state
        .db
        .call(move |conn| {
            conn.execute(
                "INSERT INTO files (id, room_id, name, mime_type, size, data, is_p2p, owner_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    file_id_clone,
                    room_id_clone,
                    file_name_clone,
                    mime_type_clone,
                    size as i64,
                    store_data,
                    is_p2p as i32,
                    owner_id_clone
                ],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    // Notify room
    if let Some(room) = state.rooms.get(&room_id) {
        let _ = room.tx.send(RoomEvent::FileAdded {
            file: file_info.clone(),
        });
    }

    (StatusCode::OK, Json(serde_json::json!({"file": file_info})))
}

async fn download_file(
    Path((room_id, file_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let file_id_clone = file_id.clone();
    let room_id_clone = room_id.clone();

    let result = state
        .db
        .call(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT name, mime_type, data, is_p2p FROM files WHERE id = ? AND room_id = ?",
            )?;
            let result: Result<(String, String, Option<Vec<u8>>, i32), _> =
                stmt.query_row(rusqlite::params![file_id_clone, room_id_clone], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                });
            Ok(result.ok())
        })
        .await
        .unwrap();

    match result {
        Some((name, mime_type, Some(data), _)) => {
            let body = Body::from(data);
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime_type)
                .header(
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{}\"", name),
                )
                .body(body)
                .unwrap()
        }
        Some((_, _, None, 1)) => {
            // P2P file - no data stored
            Response::builder()
                .status(StatusCode::NOT_FOUND)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"error": "P2P file - request from owner"}).to_string(),
                ))
                .unwrap()
        }
        _ => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({"error": "File not found"}).to_string(),
            ))
            .unwrap(),
    }
}

async fn delete_file(
    Path((room_id, file_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let file_id_clone = file_id.clone();
    let room_id_clone = room_id.clone();

    let result = state
        .db
        .call(move |conn| {
            conn.execute(
                "DELETE FROM files WHERE id = ? AND room_id = ?",
                rusqlite::params![file_id_clone, room_id_clone],
            )?;
            Ok(())
        })
        .await;

    match result {
        Ok(_) => {
            if let Some(room) = state.rooms.get(&room_id) {
                let _ = room.tx.send(RoomEvent::FileRemoved { file_id });
            }
            (StatusCode::OK, Json(serde_json::json!({"success": true})))
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

// ============================================================================
// WebSocket
// ============================================================================

async fn websocket_handler(
    ws: WebSocketUpgrade,
    Path(room_id): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, room_id, state))
}

async fn handle_socket(socket: WebSocket, room_id: String, state: AppState) {
    let user_id = Uuid::new_v4().to_string()[..8].to_string();

    let room_id_clone = room_id.clone();
    let db_content = state
        .db
        .call(move |conn| {
            let mut stmt =
                conn.prepare("SELECT content FROM rooms WHERE id = ? AND password_hash IS NULL")?;
            let result: Result<String, _> = stmt.query_row([&room_id_clone], |row| row.get(0));
            Ok(result.ok())
        })
        .await
        .unwrap();

    let room = state.rooms.entry(room_id.clone()).or_insert_with(|| {
        let (tx, _) = broadcast::channel(100);
        Room {
            content: db_content.unwrap_or_default(),
            tx,
        }
    });

    let tx = room.tx.clone();
    let initial_content = room.content.clone();
    drop(room);

    state
        .room_users
        .entry(room_id.clone())
        .or_insert_with(HashSet::new)
        .insert(user_id.clone());

    let user_count = state
        .room_users
        .get(&room_id)
        .map(|u| u.len())
        .unwrap_or(0);
    let _ = tx.send(RoomEvent::Users { count: user_count });

    let mut rx = tx.subscribe();
    let (mut sender, mut receiver) = socket.split();

    let init_msg = serde_json::json!({
        "type": "init",
        "content": initial_content,
        "userId": user_id,
        "users": user_count
    });
    let _ = sender
        .send(Message::Text(init_msg.to_string().into()))
        .await;

    let user_id_clone = user_id.clone();

    let mut send_task = tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            match &event {
                RoomEvent::Content { from, .. } if from == &user_id_clone => continue,
                RoomEvent::P2PSignal { to, .. } if to != &user_id_clone => continue,
                _ => {}
            }
            let msg = serde_json::to_string(&event).unwrap();
            if sender.send(Message::Text(msg.into())).await.is_err() {
                break;
            }
        }
    });

    let state_clone = state.clone();
    let room_id_clone = room_id.clone();
    let user_id_for_recv = user_id.clone();
    let tx_for_recv = tx.clone();

    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            if let Message::Text(text) = msg {
                if let Ok(event) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(event_type) = event.get("type").and_then(|t| t.as_str()) {
                        match event_type {
                            "content" => {
                                if let Some(content) = event.get("content").and_then(|c| c.as_str())
                                {
                                    if let Some(mut room) =
                                        state_clone.rooms.get_mut(&room_id_clone)
                                    {
                                        room.content = content.to_string();
                                    }
                                    let _ = tx_for_recv.send(RoomEvent::Content {
                                        content: content.to_string(),
                                        from: user_id_for_recv.clone(),
                                    });
                                }
                            }
                            "p2p_signal" => {
                                if let (Some(to), Some(signal)) = (
                                    event.get("to").and_then(|t| t.as_str()),
                                    event.get("signal"),
                                ) {
                                    let _ = tx_for_recv.send(RoomEvent::P2PSignal {
                                        from: user_id_for_recv.clone(),
                                        to: to.to_string(),
                                        signal: signal.clone(),
                                    });
                                }
                            }
                            "p2p_request" => {
                                if let Some(file_id) =
                                    event.get("file_id").and_then(|f| f.as_str())
                                {
                                    let _ = tx_for_recv.send(RoomEvent::P2PRequest {
                                        from: user_id_for_recv.clone(),
                                        file_id: file_id.to_string(),
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    });

    tokio::select! {
        _ = &mut send_task => recv_task.abort(),
        _ = &mut recv_task => send_task.abort(),
    }

    if let Some(mut users) = state.room_users.get_mut(&room_id) {
        users.remove(&user_id);
        let count = users.len();
        let _ = tx.send(RoomEvent::Users { count });

        if count == 0 {
            drop(users);
            state.room_users.remove(&room_id);

            // Clean up room if not protected and not persistent
            let is_temp_protected = state.temp_protections.contains_key(&room_id);

            if !is_temp_protected {
                // Check if room is in DB
                let room_id_check = room_id.clone();
                let is_persistent = state
                    .db
                    .call(move |conn| {
                        let count: i32 = conn
                            .query_row(
                                "SELECT COUNT(*) FROM rooms WHERE id = ?",
                                [&room_id_check],
                                |row| row.get(0),
                            )
                            .unwrap_or(0);
                        Ok(count > 0)
                    })
                    .await
                    .unwrap_or(false);

                if !is_persistent {
                    // Remove room from memory and delete associated files
                    state.rooms.remove(&room_id);

                    let room_id_files = room_id.clone();
                    let _ = state.db.call(move |conn| {
                        conn.execute("DELETE FROM files WHERE room_id = ?", [&room_id_files])?;
                        Ok(())
                    }).await;

                    tracing::info!("Cleaned up empty room: {}", room_id);
                }
            }
        }
    }
}
