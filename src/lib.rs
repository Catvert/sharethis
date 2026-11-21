//! ShareThis: real-time text & file sharing.

pub mod config;
mod db;
mod error;
mod files;
mod maintenance;
mod rooms;
mod security;
mod state;
mod static_files;
mod storage;
mod ws;

use std::{future::Future, net::SocketAddr};

use axum::{
    Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderValue, StatusCode, header},
    middleware,
    routing::{get, post},
};
use tokio::net::TcpListener;
use tower_http::set_header::SetResponseHeaderLayer;

pub use crate::config::Config;
use crate::{db::Db, state::AppState, storage::Storage};

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// No inline scripts: injected markup cannot execute JavaScript.
const PAGE_CSP: &str = "default-src 'self'; script-src 'self'; \
    style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; \
    object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

/// A fully initialised application (storage, migrated database, cleaned state).
pub struct App {
    state: AppState,
}

impl App {
    pub async fn init(config: Config) -> Result<Self, BoxError> {
        let storage = Storage::init(config.storage_dir.clone())
            .await
            .map_err(|e| format!("storage {}: {e}", config.storage_dir.display()))?;
        let db = Db::connect(&config.database_url)
            .await
            .map_err(|e| format!("database connection: {e}"))?;
        db.migrate(&storage)
            .await
            .map_err(|e| format!("database migration: {e}"))?;
        tracing::info!(
            "database: {:?}, file storage: {}",
            db.backend(),
            storage.root().display()
        );

        let state = AppState::new(config, db, storage);
        maintenance::startup_cleanup(&state)
            .await
            .map_err(|e| format!("startup cleanup: {}", e.message))?;
        Ok(Self { state })
    }

    /// Starts the background cleanup task and serves until `shutdown` resolves.
    pub async fn serve(
        self,
        listener: TcpListener,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> std::io::Result<()> {
        let cleanup = tokio::spawn(maintenance::run(self.state.clone()));
        let result = axum::serve(
            listener,
            router(self.state).into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown)
        .await;
        cleanup.abort();
        result
    }
}

fn router(state: AppState) -> Router {
    // Multipart framing adds a few bytes on top of the file itself.
    let upload_limit = usize::try_from(state.config.max_file_size)
        .unwrap_or(usize::MAX)
        .saturating_add(256 * 1024);

    Router::new()
        .route("/", get(rooms::index_page))
        .route("/new", get(rooms::create_room))
        .route("/healthz", get(healthz))
        .route("/static/{file}", get(static_files::serve))
        .route("/room/{room_id}", get(rooms::room_page))
        .route("/ws/{room_id}", get(ws::websocket_handler))
        .route("/api/room/{room_id}/save", post(rooms::save_room))
        .route("/api/room/{room_id}/info", get(rooms::room_info))
        .route("/api/room/{room_id}/unlock", post(rooms::unlock_room))
        .route(
            "/api/room/{room_id}/files",
            get(files::list_files)
                .post(files::upload_file)
                .layer(DefaultBodyLimit::max(upload_limit)),
        )
        .route(
            "/api/room/{room_id}/files/{file_id}",
            get(files::download_file).delete(files::delete_file),
        )
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            security::rate_limit_middleware,
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        // Room URLs are secrets: never leak them through the Referer header.
        .layer(SetResponseHeaderLayer::if_not_present(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(PAGE_CSP),
        ))
        .with_state(state)
}

async fn healthz(State(state): State<AppState>) -> (StatusCode, &'static str) {
    if state.db.ping().await {
        (StatusCode::OK, "ok")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "database unavailable")
    }
}
