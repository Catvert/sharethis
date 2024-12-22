use askama::Template;
use crate::{vite::ViteAssets, Room};

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
    pub room_id: String,
    pub content: String,
    pub updated_at: String,
    pub vite_assets: String,
}

impl RoomTemplate {
    pub fn new(room_id: String, room: Room) -> Self {
        let vite = ViteAssets::new();
        Self {
            room_id,
            content: room.content,
            updated_at: room.updated_at.format("%d-%m-%Y %H:%M:%S").to_string(),
            vite_assets: vite.get_scripts("js/room.js"),
        }
    }
}
