//! The [`DesktopProvider`] contract: how a platform backend supplies
//! desktop environment data to the neutral core.
//!
//! A provider owns its connection to the environment (Niri's IPC socket,
//! Win32 event hooks, Wayland protocol objects — its choice) and
//! translates what it learns into the neutral [`crate::model`] types and
//! [`crate::events::EnvironmentEvent`]s. Platform-specific structures
//! must never cross this boundary; the trait itself is platform-neutral
//! (no file descriptors, sockets or OS types in its signature), so the
//! same consumer code runs against any provider.
//!
//! Two delivery styles are supported, declared via
//! [`DesktopCapabilities`]:
//!
//! - **push** (`event_stream: true`): the provider holds a persistent
//!   subscription (socket, hook, ...); [`DesktopProvider::poll_events`]
//!   non-blockingly drains it. Event-loop integration is the provider
//!   crate's business (see `ocm-desktop-niri::NiriSource` for the
//!   calloop shape).
//! - **pull**: the consumer re-invokes the relevant fetches on its own
//!   schedule (document the interval and why polling is required).

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

/// A platform backend for desktop environment awareness.
pub trait DesktopProvider {
    /// Provider identity (e.g. `"niri-ipc"`, `"win32"`).
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

    /// Re-fetch the output set. For providers whose push stream does not
    /// cover output changes (the consumer schedules this; see
    /// [`DesktopCapabilities::output_events`]).
    fn refresh_outputs(&mut self) -> Result<Vec<OutputInfo>, ProviderError> {
        Err(ProviderError::Unsupported)
    }

    /// Ask the provider to invoke `wake` whenever it enqueues events.
    ///
    /// Providers that integrate with the consumer's event loop through a
    /// readiness notification (the Niri fd source) ignore this. Providers
    /// whose events arrive on a foreign thread (the Win32 hook thread)
    /// use it so a blocking wait can be woken without polling.
    fn set_wake_callback(&mut self, _wake: std::sync::Arc<dyn Fn() + Send + Sync>) {}
}
