//! # ocm-core
//!
//! Shared foundation for OpenCuteMate: configuration (TOML), shared enums,
//! error types and logging initialization.
//!
//! This crate must stay free of any windowing or rendering dependencies.

use serde::{Deserialize, Serialize};

pub mod logging;

/// Layer-shell layer the overlay surface is placed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layer {
    /// Above all normal windows and the top layer.
    Overlay,
    /// Above normal windows, below the overlay layer.
    Top,
}

impl Default for Layer {
    fn default() -> Self {
        Self::Overlay
    }
}

/// Layer-shell keyboard interactivity mode (`zwlr_layer_surface_v1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyboardInteractivity {
    /// No keyboard focus (default). The PoC only needs pointer input.
    None,
    /// Keyboard focus on demand (layer-shell v4+).
    OnDemand,
    /// Grab all keyboard input.
    Exclusive,
}

impl Default for KeyboardInteractivity {
    fn default() -> Self {
        Self::None
    }
}

/// `[window]` section.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowConfig {
    pub layer: Layer,
    pub click_through: bool,
    pub keyboard_interactivity: KeyboardInteractivity,
    /// Output to place the overlay on; empty string = first advertised output.
    pub output: String,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            layer: Layer::default(),
            click_through: true,
            keyboard_interactivity: KeyboardInteractivity::default(),
            output: String::new(),
        }
    }
}

/// `[render]` section.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RenderConfig {
    /// Frames-per-second cap for the scheduler (compositor vsync still applies).
    pub fps_limit: u32,
    /// `true` = Fifo present mode (vsync), `false` = best-effort no-vsync.
    pub vsync: bool,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self { fps_limit: 60, vsync: true }
    }
}

/// `[character]` section.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CharacterConfig {
    /// Reserved for a future model path; unused in the PoC (procedural cube).
    pub model: String,
    /// Horizontal position, normalized [0, 1], left to right.
    pub position_x: f32,
    /// Vertical position, normalized [0, 1], top to bottom.
    pub position_y: f32,
    /// Character height as a fraction of the output's logical height.
    pub size: f32,
    /// Continuous animation (bob + spin) driven by frame callbacks.
    pub animate: bool,
}

impl Default for CharacterConfig {
    fn default() -> Self {
        Self {
            model: String::new(),
            position_x: 0.5,
            position_y: 0.5,
            size: 0.4,
            animate: false,
        }
    }
}

/// Top-level OpenCuteMate configuration.
///
/// Example TOML:
///
/// ```toml
/// [window]
/// layer = "overlay"
/// click_through = true
///
/// [render]
/// fps_limit = 60
/// vsync = true
///
/// [character]
/// model = ""
/// position_x = 0.5
/// position_y = 0.5
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub window: WindowConfig,
    pub render: RenderConfig,
    pub character: CharacterConfig,
}

impl Config {
    /// Load configuration from a TOML file.
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

/// Platform-layer errors (connection, globals, surface setup).
#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("failed to connect to the Wayland display: {0}")]
    Connect(String),
    #[error("required Wayland global missing: {0}")]
    MissingGlobal(&'static str),
    #[error("output {0:?} not found")]
    OutputNotFound(String),
    #[error("the compositor did not configure the layer surface in time")]
    ConfigureTimeout,
    #[error("the layer surface was closed by the compositor")]
    Closed,
    #[error("wayland protocol error: {0}")]
    Protocol(String),
    #[error("platform failure: {0}")]
    Other(String),
}
