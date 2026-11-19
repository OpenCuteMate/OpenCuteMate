//! The [`DesktopProvider`] contract: how a compositor backend supplies
//! desktop environment data to the neutral core.
//!
//! A provider owns its connection to the compositor (IPC socket, Wayland
//! protocol objects, helper process — its choice) and translates what it
//! learns into the neutral [`crate::model`] types and
//! [`crate::events::EnvironmentEvent`]s. Compositor-specific structures
//! must never cross this boundary.
//!
//! Two delivery styles are supported, declared via
//! [`DesktopCapabilities`]:
//!
//! - **push** (`event_stream: true`): the provider holds a persistent
//!   subscription; [`DesktopProvider::poll_events`] non-blockingly drains
//!   it. Event loops register [`DesktopProvider::watch_fd`].
//! - **pull**: the consumer re-invokes the relevant fetches on its own
//!   schedule (document the interval and why polling is required).

use std::os::fd::RawFd;

use crate::events::EnvironmentEvent;
use crate::model::{DesktopCapabilities, DesktopSnapshot, OutputInfo};

/// Errors a provider can surface.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// No connection to the compositor (socket missing, refused...).
    /// Providers are optional; callers treat this as "awareness is
    /// unavailable", not as a fatal error.
    #[error("desktop environment provider unavailable")]
    Unavailable,
    /// The provider does not implement this operation (per its
    /// capabilities).
    #[error("operation not supported by this provider")]
    Unsupported,
    #[error("provider i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("provider protocol error: {0}")]
    Protocol(String),
}

/// A compositor backend for desktop environment awareness.
pub trait DesktopProvider {
    /// Provider identity (e.g. `"niri-ipc"`).
    fn name(&self) -> &'static str;

    /// What this provider can actually supply. Must be truthful: the
    /// provider is never asked for something it reports as unavailable.
    fn capabilities(&self) -> DesktopCapabilities;

    /// One-time blocking snapshot of the current environment.
    fn snapshot(&mut self) -> Result<DesktopSnapshot, ProviderError>;

    /// Non-blocking drain of pending events (empty = nothing right now).
    /// Only meaningful for providers with `event_stream`; may return
    /// [`ProviderError::Unsupported`] otherwise.
    fn poll_events(&mut self) -> Result<Vec<EnvironmentEvent>, ProviderError>;

    /// The read end to register with an event loop, if the provider
    /// pushes events. `None` = consumers must poll.
    fn watch_fd(&self) -> Option<RawFd>;

    /// Re-fetch the output set. For providers whose push stream does not
    /// cover output changes (the consumer schedules this; see
    /// [`DesktopCapabilities::output_events`]).
    fn refresh_outputs(&mut self) -> Result<Vec<OutputInfo>, ProviderError> {
        Err(ProviderError::Unsupported)
    }
}
