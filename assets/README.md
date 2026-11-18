# OpenCuteMate assets

Character assets will live here in later phases (glTF/GLB, then VRM models
and their animation sets).

The Phase 2 PoC intentionally ships **no model assets**: the rendered
character is a procedural cube generated in Rust
(`crates/ocm-renderer/src/lib.rs::cube_geometry`). This isolates the
graphics/Wayland integration from asset loading entirely — per the approved
plan, model import is deferred to a later PoC iteration (the `gltf` crate is
reserved for that step).
