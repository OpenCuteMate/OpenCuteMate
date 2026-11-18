//! # ocm-character
//!
//! The character system: everything that *is* a desktop companion — its
//! model, its state machine, its pointer interactions, its physics and
//! its animation — as pure, renderer-agnostic data and logic.
//!
//! Dependency policy (enforced by `Cargo.toml`): only `ocm-core` and the
//! platform-neutral types of `ocm-platform` (`Rect`, `PointerEvent`). No
//! Wayland, no wgpu, no Bevy. The runtime feeds platform events in and
//! reads a render-ready [`animation::AnimatedPose`] out; renderers draw
//! whatever [`model::CharacterModel`] they are handed.
//!
//! ```text
//!  runtime ──PointerEvent──► interaction::InteractionSystem
//!                                │ InteractionEvent (semantic, ElementId)
//!                                ▼
//!                       Character (state machine + physics)
//!                                │ AnimatedPose + SceneSnapshot
//!                                ▼
//!                       renderers (wgpu / bevy)
//! ```
//!
//! The input-region hitboxes flow the other way: the renderer computes
//! them from the same placement math it renders with (single source of
//! truth for "what is on screen where"), and both the interaction system
//! and the compositor input region consume them.

pub mod animation;
pub mod character;
pub mod desktop;
pub mod interaction;
pub mod model;
pub mod physics;
pub mod state;

pub use animation::{AnimatedPose, Animator, ElementPose, RootPose};
pub use character::{Character, UpdateOutcome};
pub use desktop::DesktopBounds;
pub use interaction::{ElementHitbox, InteractionEvent, InteractionSystem};
pub use model::{prototype_cube, CharacterModel, ElementId, ElementRole, ModelElement};
pub use physics::{Body, PhysicsEvent};
pub use state::Phase;
