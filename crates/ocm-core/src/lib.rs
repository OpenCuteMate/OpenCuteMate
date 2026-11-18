use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layer {
    Overlay,
    Top,
}

impl Default for Layer {
    fn default() -> Self {
        Self::Overlay
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyboardInteractivity {
    None,
    OnDemand,
}

impl Default for KeyboardInteractivity {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Position {
    pub x: f32,
    pub y: f32,
}

impl Default for Position {
    fn default() -> Self {
        Self { x: 0.5, y: 0.5 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub layer: Layer,
    pub keyboard_interactivity: KeyboardInteractivity,
    pub position: Position,
    pub character_size: f32,
    pub click_through: bool,
    pub fps_cap: u32,
    pub vsync: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            layer: Layer::default(),
            keyboard_interactivity: KeyboardInteractivity::default(),
            position: Position::default(),
            character_size: 200.0,
            click_through: true,
            fps_cap: 60,
            vsync: true,
        }
    }
}

impl Config {
    pub fn load(path: &std::path::Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::Read(path.to_path_buf(), e))?;
        let cfg: Config = toml::from_str(&text).map_err(ConfigError::Parse)?;
        Ok(cfg)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config file {0}: {1}")]
    Read(std::path::PathBuf, std::io::Error),
    #[error("failed to parse config: {0}")]
    Parse(#[from] toml::de::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("wayland connection failed: {0}")]
    Connection(String),
    #[error("missing required wayland global: {0}")]
    MissingGlobal(&'static str),
    #[error("surface creation failed: {0}")]
    SurfaceCreation(String),
    #[error("no suitable gpu adapter found")]
    NoAdapter,
}