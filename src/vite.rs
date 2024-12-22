use axum::http::HeaderValue;
use axum::response::{Html, IntoResponse};
use std::collections::HashMap;
use std::env;
use std::fs;
use serde::Deserialize;

#[derive(Deserialize)]
struct ManifestEntry {
    file: String,
    src: String,
    #[serde(default)]
    css: Vec<String>,
    #[serde(default)]
    isEntry: bool,
}

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

    fn get_manifest_entry(&self, entry_name: &str) -> Option<(String, Vec<String>)> {
        match self {
            Self::Development => None,
            Self::Production => {
                let manifest_path = "./dist/.vite/manifest.json";
                fs::read_to_string(manifest_path)
                    .ok()
                    .and_then(|content: String| -> Option<(String, Vec<String>)> {
                        let manifest: HashMap<String, ManifestEntry> =
                            serde_json::from_str(&content).ok()?;
                        manifest.get(entry_name)
                            .map(|entry| (entry.file.clone(), entry.css.clone()))
                    })
            }
        }
    }

    pub fn get_scripts(&self, entry_name: &str) -> String {
        match self {
            Self::Development => format!(
                r#"
                <script type="module" src="http://localhost:5173/@vite/client"></script>
                <script type="module" src="http://localhost:5173/{entry_name}"></script>
            "#
            ),
            Self::Production => {
                if let Some((file_path, css_files)) = self.get_manifest_entry(entry_name) {
                  let css_tags = css_files.iter()
                        .map(|css_file| format!(r#"<link rel="stylesheet" href="/{css_file}">"#))
                        .collect::<Vec<_>>()
                        .join("\n");
                    
                    format!(r#"
                        {css_tags}
                        <script type="module" src="/{file_path}"></script>
                    "#)
                } else {
                    // Entry not found in manifest.json
                    format!(
                        r#"<script>console.error("Could not find {entry_name} in manifest.json");</script>"#
                    )
                }
            }
        }
    }
}
