//! Test harness: a real server per test, on a random port.
//!
//! SQLite by default. Set `TEST_MYSQL_URL` (e.g. `mysql://root:pw@127.0.0.1:3306`,
//! a user allowed to create databases) to run the same tests against
//! MariaDB/MySQL: each test then gets its own `sharethis_test_*` database.

#![allow(dead_code)]

use std::{path::Path, time::Duration};

use futures::{SinkExt, StreamExt};
use serde_json::Value;
use sharethis::{App, Config};
use sqlx::{AnyConnection, Connection};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{self, Message, client::IntoClientRequest},
};

pub struct TestServer {
    pub base: String,
    pub config: Config,
    pub http: reqwest::Client,
    _dir: tempfile::TempDir,
    shutdown: Option<oneshot::Sender<()>>,
    handle: Option<JoinHandle<()>>,
}

async fn database_url(dir: &Path) -> String {
    let Ok(server) = std::env::var("TEST_MYSQL_URL") else {
        return format!("sqlite://{}?mode=rwc", dir.join("test.db").display());
    };
    sqlx::any::install_default_drivers();
    let name = format!("sharethis_test_{}", random_suffix());
    let mut conn = AnyConnection::connect(&server)
        .await
        .expect("connect TEST_MYSQL_URL");
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&mut conn)
        .await
        .expect("create test database");
    format!("{}/{name}", server.trim_end_matches('/'))
}

fn random_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{nanos:x}{:x}", std::process::id())
}

/// Small limits so that quotas are easy to hit.
pub async fn test_config(dir: &Path) -> Config {
    Config {
        port: 0,
        database_url: database_url(dir).await,
        storage_dir: dir.join("files"),
        max_file_size: 1_000_000,
        p2p_threshold: 500_000,
        max_content_size: 2_000,
        room_quota: 1_500_000,
        max_files_per_room: 5,
        storage_quota: 2_500_000,
        room_idle_grace: 1,
        sweep_interval: Duration::from_millis(300),
        bcrypt_cost: 4,
        ..Config::default()
    }
}

impl TestServer {
    pub async fn start() -> Self {
        Self::start_with(|_| {}).await
    }

    pub async fn start_with(customize: impl FnOnce(&mut Config)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_config(dir.path()).await;
        customize(&mut config);
        Self::launch(dir, config).await
    }

    async fn launch(dir: tempfile::TempDir, config: Config) -> Self {
        let app = App::init(config.clone()).await.expect("app init");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            app.serve(listener, async {
                let _ = rx.await;
            })
            .await
            .unwrap();
        });
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        Self {
            base,
            config,
            http,
            _dir: dir,
            shutdown: Some(tx),
            handle: Some(handle),
        }
    }

    /// Stops the server and starts a new one on the same database and files.
    pub async fn restart(mut self) -> Self {
        let _ = self.shutdown.take().unwrap().send(());
        let _ = self.handle.take().unwrap().await;
        let dir = std::mem::replace(&mut self._dir, tempfile::tempdir().unwrap());
        Self::launch(dir, self.config.clone()).await
    }

    pub fn storage_dir(&self) -> &Path {
        &self.config.storage_dir
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    pub async fn get(&self, path: &str) -> reqwest::Response {
        self.http.get(self.url(path)).send().await.unwrap()
    }

    pub async fn get_with_cookie(&self, path: &str, cookie: &str) -> reqwest::Response {
        self.http
            .get(self.url(path))
            .header("cookie", cookie)
            .send()
            .await
            .unwrap()
    }

    pub async fn delete(&self, path: &str) -> reqwest::Response {
        self.http.delete(self.url(path)).send().await.unwrap()
    }

    pub async fn post_json(&self, path: &str, body: Value) -> reqwest::Response {
        self.http
            .post(self.url(path))
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    pub async fn json(&self, path: &str) -> Value {
        self.get(path).await.json().await.unwrap()
    }

    pub async fn new_room(&self) -> String {
        let res = self.get("/new").await;
        let location = res.headers()["location"].to_str().unwrap();
        location.rsplit('/').next().unwrap().to_string()
    }

    /// Protects an active room.
    pub async fn seal(&self, room: &str, body: Value) -> reqwest::StatusCode {
        self.post_json(&format!("/api/room/{room}/save"), body)
            .await
            .status()
    }

    pub async fn unlock(&self, room: &str, password: &str) -> reqwest::Response {
        self.post_json(
            &format!("/api/room/{room}/unlock"),
            serde_json::json!({ "password": password }),
        )
        .await
    }

    /// Uploads a file; `None` when the server refused it before reading the
    /// body (connection closed while sending).
    pub async fn upload(&self, room: &str, name: &str, data: Vec<u8>) -> Option<(u16, Value)> {
        let part = reqwest::multipart::Part::bytes(data).file_name(name.to_string());
        let form = reqwest::multipart::Form::new().part("file", part);
        self.send_form(room, form).await
    }

    pub async fn announce_p2p(&self, room: &str, name: &str, size: u64) -> Option<(u16, Value)> {
        let form = reqwest::multipart::Form::new()
            .text("p2p_name", name.to_string())
            .text("p2p_size", size.to_string());
        self.send_form(room, form).await
    }

    async fn send_form(&self, room: &str, form: reqwest::multipart::Form) -> Option<(u16, Value)> {
        let res = self
            .http
            .post(self.url(&format!("/api/room/{room}/files")))
            .multipart(form)
            .send()
            .await
            .ok()?;
        let status = res.status().as_u16();
        Some((status, res.json().await.unwrap_or(Value::Null)))
    }

    pub async fn ws(&self, room: &str) -> Ws {
        let url = format!("{}/ws/{room}", self.base.replacen("http", "ws", 1));
        let (stream, _) = connect_async(url).await.expect("websocket connect");
        let mut ws = Ws {
            stream,
            pending_init: None,
        };
        let init = ws.next_json().await.expect("init message");
        assert_eq!(init["type"], "init");
        ws.pending_init = Some(init);
        ws
    }

    /// HTTP status of a refused WebSocket handshake (101 if accepted).
    pub async fn ws_status(&self, room: &str, origin: Option<&str>) -> u16 {
        let url = format!("{}/ws/{room}", self.base.replacen("http", "ws", 1));
        let mut request = url.into_client_request().unwrap();
        if let Some(origin) = origin {
            request
                .headers_mut()
                .insert("origin", origin.parse().unwrap());
        }
        match connect_async(request).await {
            Ok(_) => 101,
            Err(tungstenite::Error::Http(res)) => res.status().as_u16(),
            Err(e) => panic!("unexpected websocket error: {e}"),
        }
    }
}

pub struct Ws {
    stream: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    pub pending_init: Option<Value>,
}

impl Ws {
    pub fn init(&self) -> &Value {
        self.pending_init.as_ref().unwrap()
    }

    pub async fn send_json(&mut self, value: Value) {
        self.stream
            .send(Message::Text(value.to_string().into()))
            .await
            .unwrap();
    }

    pub async fn send_content(&mut self, content: &str) {
        self.send_json(serde_json::json!({ "type": "content", "content": content }))
            .await;
    }

    /// Next JSON message, or `None` on close / after 2 s.
    pub async fn next_json(&mut self) -> Option<Value> {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(2), self.stream.next())
                .await
                .ok()??
                .ok()?;
            match msg {
                Message::Text(text) => return serde_json::from_str(text.as_str()).ok(),
                Message::Close(_) => return None,
                _ => continue,
            }
        }
    }

    /// Waits for a message of the given type.
    pub async fn wait_for(&mut self, kind: &str) -> Option<Value> {
        while let Some(msg) = self.next_json().await {
            if msg["type"] == kind {
                return Some(msg);
            }
        }
        None
    }

    /// Close code sent by the server, once the connection ends.
    pub async fn close_code(&mut self) -> Option<u16> {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(2), self.stream.next())
                .await
                .ok()??;
            match msg {
                Ok(Message::Close(frame)) => return frame.map(|f| f.code.into()),
                Ok(_) => continue,
                Err(_) => return None,
            }
        }
    }

    /// Whether the server has closed the connection.
    pub async fn is_closed(&mut self) -> bool {
        loop {
            match tokio::time::timeout(Duration::from_secs(2), self.stream.next()).await {
                Err(_) => return false,
                Ok(None | Some(Err(_) | Ok(Message::Close(_)))) => return true,
                Ok(Some(Ok(_))) => continue,
            }
        }
    }

    pub async fn close(mut self) {
        let _ = self.stream.close(None).await;
    }
}

/// `name=value` part of the first `Set-Cookie` header.
pub fn cookie(res: &reqwest::Response) -> Option<String> {
    let header = res.headers().get("set-cookie")?.to_str().ok()?;
    Some(header.split(';').next()?.to_string())
}
