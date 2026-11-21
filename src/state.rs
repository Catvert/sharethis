use std::{collections::HashSet, sync::Arc};

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, broadcast};

use crate::{config::Config, db::Db, security::RateLimiter, storage::Storage};

/// An open (unprotected) room, alive while it has users plus a grace period.
pub struct Room {
    pub content: String,
    pub tx: broadcast::Sender<RoomEvent>,
    pub users: HashSet<String>,
    /// Unix timestamp at which the last user left.
    pub empty_since: Option<i64>,
}

impl Room {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(100);
        Self {
            content: String::new(),
            tx,
            users: HashSet::new(),
            empty_since: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RoomEvent {
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
    P2PRequest { from: String, file_id: String },
    /// The room has just been protected: clients must disconnect.
    #[serde(rename = "sealed")]
    Sealed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileInfo {
    pub id: String,
    pub name: String,
    pub size: u64,
    pub mime_type: String,
    pub is_p2p: bool,
    pub owner_id: Option<String>,
}

/// Grants access to the files of a protected room after a successful unlock.
pub struct FileToken {
    pub room_id: String,
    pub expires_at: i64,
}

#[derive(Clone)]
pub struct AppState {
    pub rooms: Arc<DashMap<String, Room>>,
    pub db: Db,
    pub storage: Storage,
    pub config: Arc<Config>,
    pub limiter: RateLimiter,
    pub file_tokens: Arc<DashMap<String, FileToken>>,
    pub ws_per_ip: Arc<DashMap<String, usize>>,
    /// Serialises upload commits (quota checks) against the orphan sweeper.
    pub upload_lock: Arc<Mutex<()>>,
}

impl AppState {
    pub fn new(config: Config, db: Db, storage: Storage) -> Self {
        Self {
            rooms: Arc::new(DashMap::new()),
            db,
            storage,
            config: Arc::new(config),
            limiter: RateLimiter::default(),
            file_tokens: Arc::new(DashMap::new()),
            ws_per_ip: Arc::new(DashMap::new()),
            upload_lock: Arc::new(Mutex::new(())),
        }
    }
}
