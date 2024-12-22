#[macro_use] extern crate log;

use axum::{
    extract::{Path, State, WebSocketUpgrade}, response::Html, routing::{delete, get, post}, serve, Router
};
use sqlx::SqlitePool;
use websocket::WsServerMessage;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::broadcast;
use tokio::sync::RwLock;
use tower_http::services::ServeDir;
use askama::Template;

mod templates;
mod websocket;
mod vite;

use templates::{IndexTemplate, RoomTemplate};

#[derive(Clone)]
struct AppState {
    db: SqlitePool,
    rooms: Arc<RwLock<HashMap<String, broadcast::Sender<WsServerMessage>>>>,
}

impl AppState {
    fn new(db: SqlitePool) -> Self {
        Self {
            db,
            rooms: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn get_or_create_room_channel(&self, room: String) -> broadcast::Sender<WsServerMessage> {
        let mut rooms = self.rooms.write().await;
        if let Some(tx) = rooms.get(&room) {
            tx.clone()
        } else {
            let (tx, _rx) = broadcast::channel(100);
            rooms.insert(room.clone(), tx.clone());
            tx
        }
    }
}

#[tokio::main]
async fn main() {
    pretty_env_logger::init();
    dotenvy::dotenv().ok();
    
    // Get database URL from environment
    let database_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set in .env file");

    // Setup database
    let db = SqlitePool::connect(&database_url)
        .await
        .expect("Failed to connect to SQLite");

    // Setup shared state
    let state = Arc::new(AppState::new(db));

    // Build router
    let app = Router::new()
        .route("/", get(root_handler))
        .route("/:room", get(room_handler))
        .route("/ws/:room", get(websocket_handler))
        .nest_service("/assets", ServeDir::new("dist/assets"))
        .with_state(state);

    // Start server
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    info!("Server running on http://localhost:3000");
    serve::serve(listener, app).await.unwrap();
}

async fn root_handler() -> Html<String> {
    let template = IndexTemplate::new();
    Html(template.render().unwrap())
}

async fn room_handler(
    Path(room): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Html<String> {
    // Get room content from database
    let content = sqlx::query_scalar!("SELECT content FROM rooms WHERE id = ?", room)
        .fetch_optional(&state.db)
        .await
        .unwrap()
        .unwrap_or_default();

    // Render template with Vite assets
    let template = RoomTemplate::new(room, content);
    Html(template.render().unwrap())
}

async fn websocket_handler(
    ws: WebSocketUpgrade,
    Path(room): Path<String>,
    State(state): State<Arc<AppState>>,
) -> impl axum::response::IntoResponse {
    ws.on_upgrade(move |socket| websocket::handle_socket(socket, room, state))
}
