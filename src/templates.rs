use askama::Template;
use crate::vite::ViteAssets;

#[derive(Template)]
#[template(path = "index.html")]
pub struct IndexTemplate {
    pub vite_assets: String,
}

impl IndexTemplate {
    pub fn new() -> Self {
        let vite = ViteAssets::new();
        Self {
            vite_assets: vite.get_scripts("js/index.js"),
        }
    }
}

#[derive(Template)]
#[template(path = "room.html")]
pub struct RoomTemplate {
    pub room: String,
    pub content: String,
    pub vite_assets: String,
}

impl RoomTemplate {
    pub fn new(room: String, content: String) -> Self {
        let vite = ViteAssets::new();
        Self {
            room,
            content,
            vite_assets: vite.get_scripts("js/room.js"),
        }
    }
}
