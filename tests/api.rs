mod common;

use std::time::Duration;

use common::{TestServer, cookie};
use serde_json::{Value, json};

const SEAL: fn(&str) -> Value = |pw| json!({ "password": pw, "max_views": 3, "expires_in": 3600 });

/// An open room with one connected client and some text.
async fn active_room(server: &TestServer, text: &str) -> (String, common::Ws) {
    let room = server.new_room().await;
    let mut ws = server.ws(&room).await;
    ws.send_content(text).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    (room, ws)
}

fn refused(result: Option<(u16, Value)>, status: u16) -> bool {
    result.is_none_or(|(s, _)| s == status)
}

// ============================================================================
// Pages, IDs, headers
// ============================================================================

#[tokio::test]
async fn room_ids_are_random_and_validated() {
    let server = TestServer::start().await;
    let room = server.new_room().await;
    assert_eq!(room.len(), 16);
    assert!(room.bytes().all(|b| b.is_ascii_alphanumeric()));
    assert_ne!(room, server.new_room().await);

    assert_eq!(server.get("/room/%3Cscript%3E").await.status(), 404);
    assert_eq!(server.get(&format!("/room/{room}")).await.status(), 200);
}

#[tokio::test]
async fn security_headers() {
    let server = TestServer::start().await;
    let res = server.get("/").await;
    let h = res.headers();
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert_eq!(h["x-frame-options"], "DENY");
    assert_eq!(h["referrer-policy"], "no-referrer");
    let csp = h["content-security-policy"].to_str().unwrap();
    assert!(csp.contains("script-src 'self';"), "{csp}");
    assert!(!csp.contains("script-src 'self' 'unsafe-inline'"));
    assert!(h.get("access-control-allow-origin").is_none());

    // No inline script left in the pages (they would be blocked by the CSP).
    let room = server.new_room().await;
    for path in ["/".to_string(), format!("/room/{room}")] {
        let html = server.get(&path).await.text().await.unwrap();
        assert!(!html.contains("<script>"), "{path} has an inline script");
        assert!(!html.contains("onclick="), "{path} has an inline handler");
    }
}

#[tokio::test]
async fn static_assets_and_healthz() {
    let server = TestServer::start().await;
    let res = server.get("/static/room.js").await;
    assert_eq!(res.status(), 200);
    assert!(
        res.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/javascript")
    );
    let etag = res.headers()["etag"].to_str().unwrap().to_string();
    let cached = server
        .http
        .get(server.url("/static/room.js"))
        .header("if-none-match", etag)
        .send()
        .await
        .unwrap();
    assert_eq!(cached.status(), 304);
    assert_eq!(server.get("/static/../Cargo.toml").await.status(), 404);
    assert_eq!(server.get("/static/nope.js").await.status(), 404);

    let health = server.get("/healthz").await;
    assert_eq!(health.status(), 200);
    assert_eq!(health.text().await.unwrap(), "ok");
}

// ============================================================================
// Open rooms
// ============================================================================

#[tokio::test]
async fn visiting_a_room_does_not_allocate_it() {
    let server = TestServer::start().await;
    let room = server.new_room().await;
    server.get(&format!("/room/{room}")).await;
    let info = server.json(&format!("/api/room/{room}/info")).await;
    assert_eq!(info["active"], false);
    assert!(refused(
        server.upload(&room, "a.txt", b"x".to_vec()).await,
        404
    ));
}

#[tokio::test]
async fn cross_origin_websocket_is_refused() {
    let server = TestServer::start().await;
    let room = server.new_room().await;
    let own = server.base.clone();
    assert_eq!(
        server.ws_status(&room, Some("http://evil.example")).await,
        403
    );
    assert_eq!(server.ws_status(&room, Some(&own)).await, 101);
}

#[tokio::test]
async fn text_is_synchronised_and_size_limited() {
    let server = TestServer::start().await;
    let (room, mut a) = active_room(&server, "hello").await;
    let mut b = server.ws(&room).await;
    assert_eq!(b.init()["content"], "hello");

    a.send_content(&"x".repeat(2_500)).await; // > max_content_size: ignored
    a.send_content("world").await;
    let update = b.wait_for("content").await.unwrap();
    assert_eq!(update["content"], "world");
}

#[tokio::test]
async fn websocket_flood_is_closed() {
    let server = TestServer::start().await;
    let (_, mut ws) = active_room(&server, "").await;
    for i in 0..150 {
        ws.send_content(&i.to_string()).await;
    }
    assert!(ws.is_closed().await);
}

#[tokio::test]
async fn reload_keeps_room_then_idle_room_is_purged() {
    let server = TestServer::start().await;
    let (room, ws) = active_room(&server, "keep me").await;
    let (status, body) = server
        .upload(&room, "k.txt", b"kept".to_vec())
        .await
        .unwrap();
    assert_eq!(status, 200);
    let file_id = body["file"]["id"].as_str().unwrap().to_string();
    ws.close().await;

    // Reconnecting quickly (page reload) keeps text and files.
    let ws = server.ws(&room).await;
    assert_eq!(ws.init()["content"], "keep me");
    let files = server.json(&format!("/api/room/{room}/files")).await;
    assert_eq!(files.as_array().unwrap().len(), 1);
    ws.close().await;

    // After the grace period, everything is gone.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let info = server.json(&format!("/api/room/{room}/info")).await;
    assert_eq!(info["active"], false);
    assert_eq!(
        server
            .get(&format!("/api/room/{room}/files"))
            .await
            .status(),
        404
    );
    assert!(!server.storage_dir().join(file_id).exists());
}

// ============================================================================
// Files
// ============================================================================

#[tokio::test]
async fn hostile_file_names_are_neutralised() {
    let server = TestServer::start().await;
    let (room, _ws) = active_room(&server, "").await;
    let name = "<img src=x onerror=alert(1)>\u{202e}é\".txt";
    let (status, body) = server
        .upload(&room, name, b"payload".to_vec())
        .await
        .unwrap();
    assert_eq!(status, 200);
    assert!(!body["file"]["name"].as_str().unwrap().contains('\u{202e}'));
    let id = body["file"]["id"].as_str().unwrap();
    assert!(server.storage_dir().join(id).is_file(), "stored on disk");

    let res = server.get(&format!("/api/room/{room}/files/{id}")).await;
    assert_eq!(res.status(), 200);
    let disposition = res.headers()["content-disposition"].to_str().unwrap();
    assert!(disposition.is_ascii());
    assert!(disposition.starts_with("attachment; filename=\""));
    assert!(disposition.contains("filename*=UTF-8''"));
    assert!(
        res.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("sandbox")
    );
    assert_eq!(res.text().await.unwrap(), "payload");
}

#[tokio::test]
async fn quotas_are_enforced() {
    let server = TestServer::start().await;
    let (room, _ws) = active_room(&server, "").await;

    assert!(refused(
        server.upload(&room, "big", vec![0; 1_000_001]).await,
        413
    ));
    assert_eq!(
        server
            .upload(&room, "q1", vec![0; 900_000])
            .await
            .unwrap()
            .0,
        200
    );
    assert_eq!(
        server
            .upload(&room, "q2", vec![0; 900_000])
            .await
            .unwrap()
            .0,
        413,
        "room quota"
    );
    for i in 0..4 {
        assert_eq!(
            server
                .upload(&room, &format!("f{i}"), vec![1])
                .await
                .unwrap()
                .0,
            200
        );
    }
    assert_eq!(
        server.upload(&room, "f5", vec![1]).await.unwrap().0,
        413,
        "max files"
    );

    // Global quota (2.5 MB), spread over rooms that are each under quota.
    let (room2, _ws2) = active_room(&server, "").await;
    let (room3, _ws3) = active_room(&server, "").await;
    assert_eq!(
        server
            .upload(&room2, "g1", vec![0; 900_000])
            .await
            .unwrap()
            .0,
        200
    );
    assert_eq!(
        server
            .upload(&room3, "g2", vec![0; 900_000])
            .await
            .unwrap()
            .0,
        507
    );
}

#[tokio::test]
async fn p2p_files_are_announced_not_uploaded() {
    let server = TestServer::start().await;
    let (room, _ws) = active_room(&server, "").await;
    let (status, body) = server
        .announce_p2p(&room, "huge.iso", 9_000_000_000)
        .await
        .unwrap();
    assert_eq!(status, 200);
    assert_eq!(body["file"]["is_p2p"], true);
    let id = body["file"]["id"].as_str().unwrap();
    assert_eq!(
        server
            .get(&format!("/api/room/{room}/files/{id}"))
            .await
            .status(),
        404
    );
}

// ============================================================================
// Protected rooms
// ============================================================================

#[tokio::test]
async fn sealing_disconnects_clients_and_cannot_be_redone() {
    let server = TestServer::start().await;
    let (room, mut a) = active_room(&server, "secret").await;
    let mut b = server.ws(&room).await;

    assert_eq!(server.seal(&room, SEAL("pw")).await, 200);
    assert!(a.wait_for("sealed").await.is_some());
    assert_eq!(b.close_code().await, Some(4001));

    // No live access to the text any more.
    assert_eq!(server.ws_status(&room, None).await, 403);
    // Nobody can overwrite the password or reset the counter.
    let again = server.seal(&room, json!({ "password": "attacker" })).await;
    assert!(again == 404 || again == 409, "{again}");
    assert_eq!(server.unlock(&room, "attacker").await.status(), 401);
    let res = server.unlock(&room, "pw").await;
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["content"], "secret");
    assert_eq!(body["views_remaining"], 2);
}

#[tokio::test]
async fn seal_validates_input() {
    let server = TestServer::start().await;
    let (room, _ws) = active_room(&server, "").await;
    assert_eq!(server.seal(&room, json!({ "password": "" })).await, 400);
    assert_eq!(
        server
            .seal(&room, json!({ "password": "x".repeat(73) }))
            .await,
        400
    );
    assert_eq!(
        server
            .seal(&room, json!({ "password": "p", "max_views": 0 }))
            .await,
        400
    );
    let inactive = server.new_room().await;
    assert_eq!(
        server.seal(&inactive, json!({ "password": "p" })).await,
        404
    );
}

#[tokio::test]
async fn protected_files_require_unlock() {
    let server = TestServer::start().await;
    let (room, _ws) = active_room(&server, "with files").await;
    let (_, body) = server
        .upload(&room, "doc.txt", b"doc".to_vec())
        .await
        .unwrap();
    let id = body["file"]["id"].as_str().unwrap().to_string();
    server
        .announce_p2p(&room, "big.iso", 9_999_999)
        .await
        .unwrap();
    assert_eq!(server.seal(&room, SEAL("pw")).await, 200);

    let files = format!("/api/room/{room}/files");
    let file = format!("{files}/{id}");
    assert_eq!(server.get(&files).await.status(), 401);
    assert_eq!(server.get(&file).await.status(), 401);
    assert_eq!(
        server
            .get_with_cookie(&file, "st_access=forged")
            .await
            .status(),
        401
    );
    assert_eq!(server.delete(&file).await.status(), 403);
    assert!(refused(server.upload(&room, "x", vec![1]).await, 403));

    let res = server.unlock(&room, "pw").await;
    let set_cookie = res.headers()["set-cookie"].to_str().unwrap().to_string();
    assert!(set_cookie.contains("HttpOnly") && set_cookie.contains("SameSite=Strict"));
    assert!(set_cookie.contains(&format!("Path=/api/room/{room}/")));
    let cookie = cookie(&res).unwrap();
    let body: Value = res.json().await.unwrap();
    let listed = body["files"].as_array().unwrap();
    assert_eq!(listed.len(), 1, "P2P files are dropped when sealing");

    let res = server.get_with_cookie(&file, &cookie).await;
    assert_eq!(res.status(), 200);
    assert_eq!(res.text().await.unwrap(), "doc");
    assert_eq!(server.get_with_cookie(&files, &cookie).await.status(), 200);

    // The cookie is bound to its room.
    let (other, _ws2) = active_room(&server, "").await;
    server.upload(&other, "o", vec![1]).await.unwrap();
    assert_eq!(server.seal(&other, SEAL("pw")).await, 200);
    assert_eq!(
        server
            .get_with_cookie(&format!("/api/room/{other}/files"), &cookie)
            .await
            .status(),
        401
    );
}

#[tokio::test]
async fn view_counter_is_atomic() {
    let server = TestServer::start().await;
    let (room, _ws) = active_room(&server, "race").await;
    assert_eq!(server.seal(&room, SEAL("pw")).await, 200);

    let attempts = (0..10).map(|_| server.unlock(&room, "pw"));
    let statuses: Vec<u16> = futures::future::join_all(attempts)
        .await
        .iter()
        .map(|r| r.status().as_u16())
        .collect();
    assert_eq!(
        statuses.iter().filter(|s| **s == 200).count(),
        3,
        "{statuses:?}"
    );
    assert!(
        statuses.iter().all(|s| *s == 200 || *s == 410),
        "{statuses:?}"
    );
    assert_eq!(server.get(&format!("/room/{room}")).await.status(), 410);
}

#[tokio::test]
async fn last_view_wipes_text_but_keeps_files_briefly() {
    let server = TestServer::start().await;
    let (room, _ws) = active_room(&server, "one shot").await;
    let (_, body) = server.upload(&room, "f.txt", b"f".to_vec()).await.unwrap();
    let id = body["file"]["id"].as_str().unwrap().to_string();
    let seal = json!({ "password": "pw", "max_views": 1 });
    assert_eq!(server.seal(&room, seal).await, 200);

    let res = server.unlock(&room, "pw").await;
    let cookie = cookie(&res).unwrap();
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["content"], "one shot");
    assert_eq!(body["last_view"], true);

    assert_eq!(server.unlock(&room, "pw").await.status(), 410);
    let file = format!("/api/room/{room}/files/{id}");
    assert_eq!(server.get_with_cookie(&file, &cookie).await.status(), 200);
}

#[tokio::test]
async fn unlock_is_rate_limited() {
    let server = TestServer::start().await;
    let (room, _ws) = active_room(&server, "").await;
    assert_eq!(server.seal(&room, json!({ "password": "pw" })).await, 200);
    let mut statuses = Vec::new();
    for i in 0..12 {
        statuses.push(
            server
                .unlock(&room, &format!("guess{i}"))
                .await
                .status()
                .as_u16(),
        );
    }
    assert_eq!(&statuses[..10], &[401; 10]);
    assert_eq!(&statuses[10..], &[429; 2]);
    // Even the right password is refused while blocked.
    assert_eq!(server.unlock(&room, "pw").await.status(), 429);
}

#[tokio::test]
async fn temporary_protection_does_not_survive_restart() {
    let server = TestServer::start().await;
    let (room, ws) = active_room(&server, "temp").await;
    let (_, body) = server.upload(&room, "t.txt", b"t".to_vec()).await.unwrap();
    let id = body["file"]["id"].as_str().unwrap().to_string();
    let seal = json!({ "password": "pw", "persistent": false });
    assert_eq!(server.seal(&room, seal).await, 200);
    ws.close().await;

    let (persistent, ws) = active_room(&server, "kept").await;
    assert_eq!(
        server.seal(&persistent, json!({ "password": "pw" })).await,
        200
    );
    ws.close().await;

    let storage = server.storage_dir().to_path_buf();
    let server = server.restart().await;
    let info = server.json(&format!("/api/room/{room}/info")).await;
    assert_eq!(info["is_protected"], false);
    assert!(!storage.join(id).exists());
    let info = server.json(&format!("/api/room/{persistent}/info")).await;
    assert_eq!(info["is_protected"], true);
}
