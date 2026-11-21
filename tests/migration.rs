//! Upgrading a v0.1 SQLite database (files stored as BLOBs).

mod common;

use serde_json::Value;
use sqlx::{AnyConnection, Connection};

#[tokio::test]
async fn legacy_sqlite_database_is_migrated() {
    if std::env::var("TEST_MYSQL_URL").is_ok() {
        return; // v0.1 only supported SQLite
    }
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("legacy.db");
    let url = format!("sqlite://{}?mode=rwc", db_path.display());

    sqlx::any::install_default_drivers();
    let mut conn = AnyConnection::connect(&url).await.unwrap();
    for stmt in [
        "CREATE TABLE rooms (id TEXT PRIMARY KEY, content TEXT NOT NULL, password_hash TEXT,
            max_views INTEGER, current_views INTEGER DEFAULT 0, expires_at INTEGER,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP)",
        "CREATE TABLE files (id TEXT PRIMARY KEY, room_id TEXT NOT NULL, name TEXT NOT NULL,
            mime_type TEXT NOT NULL, size INTEGER NOT NULL, data BLOB, is_p2p INTEGER DEFAULT 0,
            owner_id TEXT, created_at DATETIME DEFAULT CURRENT_TIMESTAMP)",
    ] {
        sqlx::query(stmt).execute(&mut conn).await.unwrap();
    }
    let hash = bcrypt::hash("pw", 4).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    for (id, expires_at) in [("legacy01", now + 3600), ("expired1", now - 10)] {
        sqlx::query("INSERT INTO rooms (id, content, password_hash, max_views, current_views, expires_at) VALUES (?, 'secret text', ?, 2, 0, ?)")
            .bind(id)
            .bind(hash.as_str())
            .bind(expires_at)
            .execute(&mut conn)
            .await
            .unwrap();
    }
    for (id, room) in [
        ("aaaa1111", "legacy01"),
        ("bbbb2222", "gone0000"),
        ("cccc3333", "expired1"),
    ] {
        sqlx::query("INSERT INTO files (id, room_id, name, mime_type, size, data, is_p2p) VALUES (?, ?, 'hello.txt', 'text/plain', 5, ?, 0)")
            .bind(id)
            .bind(room)
            .bind(b"hello".to_vec())
            .execute(&mut conn)
            .await
            .unwrap();
    }
    conn.close().await.unwrap();

    let mut config = common::test_config(dir.path()).await;
    config.database_url = url.clone();
    let storage = config.storage_dir.clone();
    sharethis::App::init(config).await.expect("migration");

    // BLOB moved to disk; orphan and expired-room files dropped.
    assert_eq!(std::fs::read(storage.join("aaaa1111")).unwrap(), b"hello");
    assert!(!storage.join("bbbb2222").exists());
    assert!(!storage.join("cccc3333").exists());

    let mut conn = AnyConnection::connect(&url).await.unwrap();
    assert!(
        sqlx::query("SELECT data FROM files")
            .fetch_all(&mut conn)
            .await
            .is_err(),
        "data column dropped"
    );
    let rooms: Vec<String> = sqlx::query_scalar("SELECT id FROM rooms")
        .fetch_all(&mut conn)
        .await
        .unwrap();
    assert_eq!(rooms, ["legacy01"]);
    conn.close().await.unwrap();

    // The legacy room still works end to end.
    let server = common::TestServer::start_with(|c| {
        c.database_url = url.clone();
        c.storage_dir = storage.clone();
    })
    .await;
    let res = server.unlock("legacy01", "pw").await;
    assert_eq!(res.status(), 200);
    let cookie = common::cookie(&res).unwrap();
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["content"], "secret text");
    assert_eq!(body["views_remaining"], 1);
    let file = server
        .get_with_cookie("/api/room/legacy01/files/aaaa1111", &cookie)
        .await;
    assert_eq!(file.text().await.unwrap(), "hello");
}
