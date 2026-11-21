//! Front-end scripts, embedded in the binary.

use std::hash::{DefaultHasher, Hash, Hasher};

use axum::{
    extract::Path,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};

fn asset(name: &str) -> Option<&'static str> {
    Some(match name {
        "common.js" => include_str!("../static/common.js"),
        "index.js" => include_str!("../static/index.js"),
        "protected.js" => include_str!("../static/protected.js"),
        "room.js" => include_str!("../static/room.js"),
        _ => return None,
    })
}

pub async fn serve(Path(file): Path<String>, headers: HeaderMap) -> Response {
    let Some(body) = asset(&file) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let mut hasher = DefaultHasher::new();
    body.hash(&mut hasher);
    let etag = format!("\"{:016x}\"", hasher.finish());

    let cached = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag));
    let common = [
        (header::CACHE_CONTROL, "no-cache".to_string()),
        (header::ETAG, etag),
    ];
    if cached {
        return (StatusCode::NOT_MODIFIED, common).into_response();
    }
    (
        common,
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        body,
    )
        .into_response()
}
