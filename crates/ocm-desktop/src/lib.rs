//! # ocm-desktop
//!
//! Compositor-neutral model of the desktop environment OpenCuteMate's
//! character lives in: displays, work areas, windows, workspaces — plus
//! the coordinate spaces to reason about all of them and a provider
//! interface that compositor backends implement.
//!
//! Design rules (enforced by the module layout):
//!
//! - Nothing here knows about Wayland, Niri, Bevy or any GPU stack. A
//!   provider (`ocm-desktop-niri` today) translates its compositor's API
//!   into [`model`] types and [`events::EnvironmentEvent`]s.
//! - Information a provider cannot obtain is represented as `None` /
//!   `false`, never guessed. The character crate and the runtime consume
//!   only what actually arrived.
//! - The desktop is a **collection of output regions**, never an assumed
//!   single rectangle ([`layout::DesktopLayout`]).
//!
//! Consumers:
//!
//! - `ocm-runtime` owns a [`environment::DesktopEnvironment`], feeds it
//!   provider events, derives the character's bounds from work areas and
//!   answers "what is near the character" questions.
//! - Future character behaviors read the same neutral snapshot — they
//!   never talk to a compositor directly.

pub mod environment;
pub mod events;
pub mod layout;
pub mod model;
pub mod provider;

pub use environment::DesktopEnvironment;
pub use events::{EnvironmentChange, EnvironmentEvent};
pub use layout::DesktopLayout;
pub use model::{
    DesktopCapabilities, DesktopSnapshot, OutputInfo, OutputPhysical, OutputTransform,
    WindowGeometry, WindowInfo, WindowState, WorkspaceInfo,
};
pub use provider::{DesktopProvider, ProviderError};
