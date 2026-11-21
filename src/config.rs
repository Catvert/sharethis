use std::{env, path::PathBuf, str::FromStr, time::Duration};

const MB: u64 = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Config {
    pub port: u16,
    /// `sqlite://path?mode=rwc`, `mysql://user:pass@host/db` or `mariadb://...`
    pub database_url: String,
    /// Directory where uploaded files are stored (one file per upload).
    pub storage_dir: PathBuf,
    /// Max size of a file stored on the server.
    pub max_file_size: u64,
    /// Files larger than this are shared peer-to-peer (never uploaded).
    pub p2p_threshold: u64,
    /// Max size (bytes) of the shared text of a room.
    pub max_content_size: usize,
    /// Max bytes stored on the server per room.
    pub room_quota: u64,
    /// Max number of files (stored + P2P) per room.
    pub max_files_per_room: i64,
    /// Max bytes stored on the server, all rooms combined.
    pub storage_quota: u64,
    /// Max number of rooms kept in memory.
    pub max_rooms: usize,
    /// Max simultaneous WebSocket connections per client IP.
    pub max_ws_per_ip: usize,
    /// Seconds an empty room (text + files) is kept before being deleted.
    pub room_idle_grace: i64,
    /// Period of the background cleanup task.
    pub sweep_interval: Duration,
    /// Read the client IP from `X-Forwarded-For` (only behind a reverse proxy).
    pub trust_proxy: bool,
    /// Add the `Secure` attribute to cookies (HTTPS deployments).
    pub cookie_secure: bool,
    /// bcrypt work factor for room passwords (4..=31).
    pub bcrypt_cost: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: 3000,
            database_url: "sqlite://sharethis.db?mode=rwc".to_string(),
            storage_dir: PathBuf::from("data/files"),
            max_file_size: 10 * MB,
            p2p_threshold: 5 * MB,
            max_content_size: 1024 * 1024,
            room_quota: 100 * MB,
            max_files_per_room: 50,
            storage_quota: 5 * 1024 * MB,
            max_rooms: 10_000,
            max_ws_per_ip: 20,
            room_idle_grace: 300,
            sweep_interval: Duration::from_secs(30),
            trust_proxy: false,
            cookie_secure: false,
            bcrypt_cost: 12,
        }
    }
}

fn env_parse<T: FromStr>(key: &str) -> Option<T> {
    env::var(key).ok().and_then(|v| v.trim().parse().ok())
}

fn env_bool(key: &str) -> Option<bool> {
    env::var(key).ok().map(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

impl Config {
    /// Defaults overridden by environment variables (see `.env.example`).
    pub fn from_env() -> Self {
        let d = Self::default();

        let database_url = env::var("DATABASE_URL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| {
                env::var("DATABASE_PATH")
                    .ok()
                    .map(|path| format!("sqlite://{path}?mode=rwc"))
            })
            .unwrap_or(d.database_url);

        Self {
            port: env_parse("PORT").unwrap_or(d.port),
            database_url,
            storage_dir: env::var("STORAGE_DIR")
                .map(PathBuf::from)
                .unwrap_or(d.storage_dir),
            max_file_size: env_parse("MAX_FILE_SIZE").unwrap_or(d.max_file_size),
            p2p_threshold: env_parse("P2P_THRESHOLD").unwrap_or(d.p2p_threshold),
            max_content_size: env_parse("MAX_CONTENT_SIZE").unwrap_or(d.max_content_size),
            room_quota: env_parse("ROOM_QUOTA").unwrap_or(d.room_quota),
            max_files_per_room: env_parse("MAX_FILES_PER_ROOM").unwrap_or(d.max_files_per_room),
            storage_quota: env_parse("STORAGE_QUOTA").unwrap_or(d.storage_quota),
            max_rooms: env_parse("MAX_ROOMS").unwrap_or(d.max_rooms),
            max_ws_per_ip: env_parse("MAX_WS_PER_IP").unwrap_or(d.max_ws_per_ip),
            room_idle_grace: env_parse("ROOM_IDLE_GRACE").unwrap_or(d.room_idle_grace),
            sweep_interval: env_parse("SWEEP_INTERVAL")
                .filter(|s: &u64| *s > 0)
                .map(Duration::from_secs)
                .unwrap_or(d.sweep_interval),
            trust_proxy: env_bool("TRUST_PROXY").unwrap_or(d.trust_proxy),
            cookie_secure: env_bool("COOKIE_SECURE").unwrap_or(d.cookie_secure),
            bcrypt_cost: env_parse("BCRYPT_COST")
                .unwrap_or(d.bcrypt_cost)
                .clamp(4, 31),
        }
    }
}
