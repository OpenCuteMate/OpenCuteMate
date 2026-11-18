use ocm_core::{Config, PlatformError};
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};

pub trait SurfaceHandle {
    fn raw_window_handle(&self) -> RawWindowHandle;
    fn raw_display_handle(&self) -> RawDisplayHandle;
    fn logical_size(&self) -> (i32, i32);
}

pub trait OverlayWindow {
    fn set_input_region(&self, rects: &[Rect]);
    fn set_click_through(&self, enabled: bool);
}

pub trait OverlaySurface: OverlayWindow + SurfaceHandle {}

#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

pub trait Platform {
    fn create_overlay(&self, config: &Config) -> Result<Box<dyn OverlaySurface>, PlatformError>;
}