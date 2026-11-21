//! Database access through sqlx's `Any` driver: SQLite or MariaDB/MySQL.
//!
//! Only portable SQL is used (`?` placeholders, no upsert). Integers are
//! always read as `i64`; aggregates are cast on MySQL where `SUM` returns
//! `DECIMAL`, a type the `Any` driver does not support.

use std::{collections::HashSet, time::Duration};

use sqlx::{AnyPool, AssertSqlSafe, Row, any::AnyPoolOptions};

use crate::{security::is_safe_id, state::FileInfo, storage::Storage};

use crate::BoxError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Sqlite,
    MySql,
}

#[derive(Clone)]
pub struct Db {
    pool: AnyPool,
    backend: Backend,
}

pub struct RoomRow {
    pub content: String,
    pub password_hash: Option<String>,
    pub max_views: Option<i64>,
    pub current_views: i64,
    pub expires_at: Option<i64>,
    pub persistent: bool,
}

impl RoomRow {
    pub fn is_expired(&self, now: i64) -> bool {
        self.expires_at.is_some_and(|e| now >= e)
    }

    pub fn views_exhausted(&self) -> bool {
        self.max_views.is_some_and(|m| self.current_views >= m)
    }
}

pub struct FileRecord {
    pub name: String,
    pub mime_type: String,
    pub is_p2p: bool,
}

const SQLITE_SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS rooms (
        id TEXT PRIMARY KEY,
        content TEXT NOT NULL,
        password_hash TEXT,
        max_views INTEGER,
        current_views INTEGER NOT NULL DEFAULT 0,
        expires_at INTEGER,
        persistent INTEGER NOT NULL DEFAULT 1,
        created_at DATETIME DEFAULT CURRENT_TIMESTAMP
    )",
    "CREATE TABLE IF NOT EXISTS files (
        id TEXT PRIMARY KEY,
        room_id TEXT NOT NULL,
        name TEXT NOT NULL,
        mime_type TEXT NOT NULL,
        size INTEGER NOT NULL,
        is_p2p INTEGER NOT NULL DEFAULT 0,
        owner_id TEXT,
        created_at DATETIME DEFAULT CURRENT_TIMESTAMP
    )",
    "CREATE INDEX IF NOT EXISTS idx_files_room ON files(room_id)",
];

// utf8mb4_bin: IDs are case-sensitive (base62).
const MYSQL_SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS rooms (
        id VARCHAR(64) NOT NULL PRIMARY KEY,
        content MEDIUMTEXT NOT NULL,
        password_hash VARCHAR(255) NULL,
        max_views BIGINT NULL,
        current_views BIGINT NOT NULL DEFAULT 0,
        expires_at BIGINT NULL,
        persistent BIGINT NOT NULL DEFAULT 1,
        created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
        INDEX idx_rooms_expires (expires_at)
    ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin",
    "CREATE TABLE IF NOT EXISTS files (
        id VARCHAR(64) NOT NULL PRIMARY KEY,
        room_id VARCHAR(64) NOT NULL,
        name VARCHAR(255) NOT NULL,
        mime_type VARCHAR(255) NOT NULL,
        size BIGINT NOT NULL,
        is_p2p BIGINT NOT NULL DEFAULT 0,
        owner_id VARCHAR(64) NULL,
        created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
        INDEX idx_files_room (room_id)
    ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin",
];

const FILE_COLUMNS: &str = "id, name, mime_type, size, is_p2p, owner_id";

fn file_info(row: &sqlx::any::AnyRow) -> Result<FileInfo, sqlx::Error> {
    Ok(FileInfo {
        id: row.try_get(0)?,
        name: row.try_get(1)?,
        mime_type: row.try_get(2)?,
        size: u64::try_from(row.try_get::<i64, _>(3)?).unwrap_or(0),
        is_p2p: row.try_get::<i64, _>(4)? != 0,
        owner_id: row.try_get(5)?,
    })
}

/// MySQL reports `TEXT` columns as `BLOB`, which the `Any` driver refuses to
/// decode as `String`: fall back to the raw (UTF-8) bytes.
fn get_text(row: &sqlx::any::AnyRow, index: usize) -> Result<String, sqlx::Error> {
    match row.try_get::<String, _>(index) {
        Ok(s) => Ok(s),
        Err(sqlx::Error::ColumnDecode { .. }) => {
            let bytes: Vec<u8> = row.try_get(index)?;
            String::from_utf8(bytes).map_err(|e| sqlx::Error::ColumnDecode {
                index: index.to_string(),
                source: Box::new(e),
            })
        }
        Err(e) => Err(e),
    }
}

impl Db {
    pub async fn connect(url: &str) -> Result<Self, sqlx::Error> {
        sqlx::any::install_default_drivers();

        let (url, backend) = if let Some(rest) = url.strip_prefix("mariadb://") {
            (format!("mysql://{rest}"), Backend::MySql)
        } else if url.starts_with("mysql://") {
            (url.to_string(), Backend::MySql)
        } else if url.starts_with("sqlite:") {
            (url.to_string(), Backend::Sqlite)
        } else {
            return Err(sqlx::Error::Configuration(
                "DATABASE_URL must start with sqlite:, mysql:// or mariadb://".into(),
            ));
        };

        let pool = AnyPoolOptions::new()
            .max_connections(match backend {
                Backend::Sqlite => 4,
                Backend::MySql => 10,
            })
            .acquire_timeout(Duration::from_secs(10))
            .connect(&url)
            .await?;

        Ok(Self { pool, backend })
    }

    pub async fn ping(&self) -> bool {
        sqlx::query("SELECT 1").execute(&self.pool).await.is_ok()
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    fn sum(&self, expr: &str) -> String {
        match self.backend {
            Backend::Sqlite => format!("COALESCE(SUM({expr}), 0)"),
            Backend::MySql => format!("CAST(COALESCE(SUM({expr}), 0) AS SIGNED)"),
        }
    }

    // ------------------------------------------------------------------------
    // Schema
    // ------------------------------------------------------------------------

    pub async fn migrate(&self, storage: &Storage) -> Result<(), BoxError> {
        let schema = match self.backend {
            Backend::Sqlite => SQLITE_SCHEMA,
            Backend::MySql => MYSQL_SCHEMA,
        };
        for stmt in schema {
            sqlx::query(*stmt).execute(&self.pool).await?;
        }

        // Databases created by v0.1 lack `rooms.persistent`...
        if sqlx::query("SELECT persistent FROM rooms LIMIT 1")
            .fetch_optional(&self.pool)
            .await
            .is_err()
        {
            tracing::info!("migration: adding rooms.persistent");
            sqlx::query("ALTER TABLE rooms ADD COLUMN persistent INTEGER NOT NULL DEFAULT 1")
                .execute(&self.pool)
                .await?;
        }

        // ...and store file contents as BLOBs.
        if sqlx::query("SELECT data FROM files LIMIT 1")
            .fetch_optional(&self.pool)
            .await
            .is_ok()
        {
            self.migrate_blobs(storage).await?;
        }
        Ok(())
    }

    async fn migrate_blobs(&self, storage: &Storage) -> Result<(), BoxError> {
        let ids: Vec<String> = sqlx::query("SELECT id FROM files WHERE data IS NOT NULL")
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(|r| r.try_get(0))
            .collect::<Result<_, _>>()?;

        if !ids.is_empty() {
            tracing::info!(
                "migration: moving {} file(s) from the database to {}",
                ids.len(),
                storage.root().display()
            );
        }
        for id in &ids {
            if !is_safe_id(id) {
                tracing::warn!("migration: dropping file with invalid id {id:?}");
                self.delete_file_row(id).await?;
                continue;
            }
            let row = sqlx::query("SELECT data FROM files WHERE id = ?")
                .bind(id.as_str())
                .fetch_one(&self.pool)
                .await?;
            let data: Vec<u8> = row.try_get(0)?;
            storage.write_all(id, &data).await?;
            sqlx::query("UPDATE files SET data = NULL WHERE id = ?")
                .bind(id.as_str())
                .execute(&self.pool)
                .await?;
        }

        match sqlx::query("ALTER TABLE files DROP COLUMN data")
            .execute(&self.pool)
            .await
        {
            Ok(_) => {
                if !ids.is_empty() && self.backend == Backend::Sqlite {
                    sqlx::query("VACUUM").execute(&self.pool).await?;
                }
            }
            Err(e) => tracing::warn!("migration: could not drop files.data: {e}"),
        }
        Ok(())
    }

    // ------------------------------------------------------------------------
    // Rooms
    // ------------------------------------------------------------------------

    pub async fn get_room(&self, id: &str) -> Result<Option<RoomRow>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT content, password_hash, max_views, current_views, expires_at, persistent
             FROM rooms WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        row.map(|r| {
            Ok(RoomRow {
                content: get_text(&r, 0)?,
                password_hash: r.try_get(1)?,
                max_views: r.try_get(2)?,
                current_views: r.try_get::<Option<i64>, _>(3)?.unwrap_or(0),
                expires_at: r.try_get(4)?,
                persistent: r.try_get::<i64, _>(5)? != 0,
            })
        })
        .transpose()
    }

    /// Returns `false` if the room is already protected.
    pub async fn insert_room(
        &self,
        id: &str,
        content: &str,
        password_hash: &str,
        max_views: Option<i64>,
        expires_at: i64,
        persistent: bool,
    ) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            "INSERT INTO rooms (id, content, password_hash, max_views, current_views, expires_at, persistent)
             VALUES (?, ?, ?, ?, 0, ?, ?)",
        )
        .bind(id)
        .bind(content)
        .bind(password_hash)
        .bind(max_views)
        .bind(expires_at)
        .bind(i64::from(persistent))
        .execute(&self.pool)
        .await;

        match result {
            Ok(_) => Ok(true),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Atomically counts one view; `false` if no view is left (or expired).
    pub async fn consume_view(&self, id: &str, now: i64) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            "UPDATE rooms SET current_views = COALESCE(current_views, 0) + 1
             WHERE id = ?
               AND max_views IS NOT NULL
               AND COALESCE(current_views, 0) < max_views
               AND (expires_at IS NULL OR expires_at > ?)",
        )
        .bind(id)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// After the last view: wipe the text, keep the files until `expires_at`.
    pub async fn exhaust_room(&self, id: &str, expires_at: i64) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE rooms SET content = '', expires_at = ? WHERE id = ?")
            .bind(expires_at)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delete_room(&self, id: &str) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM rooms WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn expired_room_ids(&self, now: i64) -> Result<Vec<String>, sqlx::Error> {
        sqlx::query("SELECT id FROM rooms WHERE expires_at IS NOT NULL AND expires_at <= ?")
            .bind(now)
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(|r| r.try_get(0))
            .collect()
    }

    /// Temporary protections do not survive a restart.
    pub async fn delete_temp_rooms(&self) -> Result<u64, sqlx::Error> {
        Ok(sqlx::query("DELETE FROM rooms WHERE persistent = 0")
            .execute(&self.pool)
            .await?
            .rows_affected())
    }

    // ------------------------------------------------------------------------
    // Files
    // ------------------------------------------------------------------------

    pub async fn list_files(
        &self,
        room_id: &str,
        include_p2p: bool,
    ) -> Result<Vec<FileInfo>, sqlx::Error> {
        let sql = format!(
            "SELECT {FILE_COLUMNS} FROM files WHERE room_id = ? {} ORDER BY created_at, id",
            if include_p2p { "" } else { "AND is_p2p = 0" }
        );
        sqlx::query(AssertSqlSafe(sql))
            .bind(room_id)
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(file_info)
            .collect()
    }

    pub async fn get_file(
        &self,
        room_id: &str,
        file_id: &str,
    ) -> Result<Option<FileRecord>, sqlx::Error> {
        let row =
            sqlx::query("SELECT name, mime_type, is_p2p FROM files WHERE id = ? AND room_id = ?")
                .bind(file_id)
                .bind(room_id)
                .fetch_optional(&self.pool)
                .await?;
        row.map(|r| {
            Ok(FileRecord {
                name: r.try_get(0)?,
                mime_type: r.try_get(1)?,
                is_p2p: r.try_get::<i64, _>(2)? != 0,
            })
        })
        .transpose()
    }

    pub async fn insert_file(&self, room_id: &str, file: &FileInfo) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO files (id, room_id, name, mime_type, size, is_p2p, owner_id)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(file.id.as_str())
        .bind(room_id)
        .bind(file.name.as_str())
        .bind(file.mime_type.as_str())
        .bind(i64::try_from(file.size).unwrap_or(i64::MAX))
        .bind(i64::from(file.is_p2p))
        .bind(file.owner_id.as_deref())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_file_row(&self, file_id: &str) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM files WHERE id = ?")
            .bind(file_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// (number of files, bytes stored on the server) for a room.
    pub async fn room_usage(&self, room_id: &str) -> Result<(i64, i64), sqlx::Error> {
        let sql = format!(
            "SELECT COUNT(*), {} FROM files WHERE room_id = ?",
            self.sum("CASE WHEN is_p2p = 0 THEN size ELSE 0 END")
        );
        let row = sqlx::query(AssertSqlSafe(sql))
            .bind(room_id)
            .fetch_one(&self.pool)
            .await?;
        Ok((row.try_get(0)?, row.try_get(1)?))
    }

    /// Bytes stored on the server, all rooms combined.
    pub async fn total_usage(&self) -> Result<i64, sqlx::Error> {
        let sql = format!("SELECT {} FROM files WHERE is_p2p = 0", self.sum("size"));
        sqlx::query(AssertSqlSafe(sql))
            .fetch_one(&self.pool)
            .await?
            .try_get(0)
    }

    /// (id, is_p2p) of every file of a room.
    pub async fn room_file_ids(&self, room_id: &str) -> Result<Vec<(String, bool)>, sqlx::Error> {
        sqlx::query("SELECT id, is_p2p FROM files WHERE room_id = ?")
            .bind(room_id)
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get::<i64, _>(1)? != 0)))
            .collect()
    }

    /// (id, room_id, is_p2p) of files whose room is not in the database.
    pub async fn files_without_db_room(&self) -> Result<Vec<(String, String, bool)>, sqlx::Error> {
        sqlx::query(
            "SELECT id, room_id, is_p2p FROM files WHERE room_id NOT IN (SELECT id FROM rooms)",
        )
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| Ok((r.try_get(0)?, r.try_get(1)?, r.try_get::<i64, _>(2)? != 0)))
        .collect()
    }

    pub async fn stored_file_ids(&self) -> Result<HashSet<String>, sqlx::Error> {
        sqlx::query("SELECT id FROM files WHERE is_p2p = 0")
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(|r| r.try_get(0))
            .collect()
    }
}
