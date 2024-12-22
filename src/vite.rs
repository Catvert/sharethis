use axum::http::HeaderValue;
use axum::response::{Html, IntoResponse};
use std::env;

pub enum ViteAssets {
    Development,
    Production,
}

impl ViteAssets {
    pub fn new() -> Self {
        match env::var("RUST_ENV") {
            Ok(v) if v == "production" => Self::Production,
            _ => Self::Development,
        }
    }

    pub fn get_scripts(&self, main_js: &str) -> String {
        match self {
            Self::Development => format!(r#"
                <script type="module" src="http://localhost:5173/@vite/client"></script>
                <script type="module" src="http://localhost:5173/js/{main_js}"></script>
            "#),
            Self::Production => format!(r#"
                <script type="module" src="/assets/{main_js}"></script>
            "#),
        }
    }
}
