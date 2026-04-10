//! Persistent config + XDG paths. Env vars override the TOML file.

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    pub api_id: Option<i32>,
    pub api_hash: Option<String>,
    #[serde(default)]
    pub media: MediaConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MediaConfig {
    /// External video player argv. Examples:
    ///   player = ["mpv"]
    ///   player = ["mpv", "--vo=kitty"]
    ///   player = ["vlc", "--fullscreen"]
    /// Overridden by the `VT_PLAYER` env var (split on whitespace).
    pub player: Option<Vec<String>>,
}

#[allow(dead_code)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub config_file: PathBuf,
    pub session_file: PathBuf,
}

impl Config {
    pub fn paths() -> Paths {
        let dirs = ProjectDirs::from("dev", "vimtelegam", "vim-telegam");
        let (cfg, data) = match &dirs {
            Some(d) => (d.config_dir().to_path_buf(), d.data_dir().to_path_buf()),
            None => {
                let base = std::env::temp_dir().join("vim-telegam");
                (base.clone(), base)
            }
        };
        Paths {
            config_file: cfg.join("config.toml"),
            session_file: data.join("telegram.session"),
            config_dir: cfg,
            data_dir: data,
        }
    }

    pub fn load() -> Self {
        let path = Self::paths().config_file;
        let mut cfg: Config = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default();

        if let Ok(v) = std::env::var("TG_API_ID") {
            if let Ok(n) = v.parse() {
                cfg.api_id = Some(n);
            }
        }
        if let Ok(v) = std::env::var("TG_API_HASH") {
            cfg.api_hash = Some(v);
        }
        cfg
    }

    /// Resolve the video player argv: env var wins (split on whitespace),
    /// otherwise the config file, otherwise `mpv` with no args.
    pub fn player_argv(&self) -> Vec<String> {
        if let Ok(v) = std::env::var("VT_PLAYER") {
            let parts: Vec<String> = v.split_whitespace().map(String::from).collect();
            if !parts.is_empty() {
                return parts;
            }
        }
        if let Some(p) = &self.media.player {
            if !p.is_empty() {
                return p.clone();
            }
        }
        vec!["mpv".to_string()]
    }

    #[allow(dead_code)]
    pub fn has_credentials(&self) -> bool {
        self.api_id.is_some() && self.api_hash.is_some()
    }
}
