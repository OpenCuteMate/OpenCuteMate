//! Semantic character model: the representation that links interaction,
//! behavior and rendering to *named character elements* instead of
//! hardcoded geometry.
//!
//! The prototype cube is described as data here (see [`prototype_cube`]):
//! a tree of [`ModelElement`]s, each carrying a semantic [`ElementRole`],
//! optional visual parts and an interaction hit extent. A future imported
//! model (GLB/glTF node or VRM bone) maps onto the same structure — the
//! interaction and behavior systems only ever see [`ElementId`]s.
//!
//! Two orderings matter and are explicit:
//!
//! * **Render order** — elements are stored parents-before-children; a
//!   renderer paints them in storage order (painter's algorithm).
//! * **Hit priority** — [`CharacterModel::hit_priority`] returns
//!   interactive elements children-before-parents; the interaction system
//!   tests in that order so small embedded elements (e.g. the eyes) win
//!   over the body they sit on.

use std::fmt;

/// Semantic role of a model element. Characters map their parts onto these
/// roles; behavior systems may react to the role without knowing the mesh.
///
/// `Custom` covers everything else (e.g. the prototype cube's eyes, ears,
/// accessories); future characters are not forced into the fixed set.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ElementRole {
    /// Model root / anchor. Never interactive.
    Root,
    Head,
    Body,
    LeftArm,
    RightArm,
    LeftLeg,
    RightLeg,
    Tail,
    Custom(String),
}

/// Stable identifier of one element of a [`CharacterModel`].
///
/// An index into the model's element table, valid only against the model it
/// came from. Interaction events carry these — never renderer entities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ElementId(pub u32);

impl fmt::Display for ElementId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "element:{}", self.0)
    }
}

/// One axis-aligned visual box, in element-local units where `1.0` is the
/// character's total height.
///
/// The prototype model is built from boxes; a future importer produces
/// meshes in the same slot (see `docs/character-architecture.md`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VisualPart {
    /// Offset from the element origin (element-local units).
    pub offset: [f32; 3],
    /// Full extent of the box (element-local units).
    pub size: [f32; 3],
    /// Base sRGB color.
    pub color: [f32; 3],
}

/// A semantic element: an interaction/animation addressable node owning
/// visual parts.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelElement {
    pub id: ElementId,
    pub role: ElementRole,
    /// Human-readable name (for logs/tests; not a stable API).
    pub name: String,
    /// Parent element, `None` for the root.
    pub parent: Option<ElementId>,
    /// Offset from the parent origin, in character-height units.
    pub offset: [f32; 3],
    /// Visual boxes painted with the element (may be empty, e.g. Root).
    pub parts: Vec<VisualPart>,
    /// Interaction hit extent: half-extents of the interactive region in
    /// element-local units. `None` marks a non-interactive element.
    pub hit_extent: Option<[f32; 2]>,
}

impl ModelElement {
    pub fn is_interactive(&self) -> bool {
        self.hit_extent.is_some()
    }
}

/// A character model: an ordered element tree (see module docs for the
/// ordering contracts).
#[derive(Debug, Clone, PartialEq)]
pub struct CharacterModel {
    elements: Vec<ModelElement>,
}

impl CharacterModel {
    /// Build a model from elements in render order. The first element must
    /// be the root (no parent).
    pub fn new(elements: Vec<ModelElement>) -> Self {
        debug_assert!(
            elements.first().is_some_and(|e| e.parent.is_none()),
            "the first element must be the root"
        );
        Self { elements }
    }

    pub fn elements(&self) -> &[ModelElement] {
        &self.elements
    }

    pub fn element(&self, id: ElementId) -> &ModelElement {
        &self.elements[id.0 as usize]
    }

    /// Interactive elements in **hit-priority order** (children before
    /// parents): the first containing rect wins during hit-testing.
    pub fn hit_priority(&self) -> impl Iterator<Item = &ModelElement> {
        self.elements.iter().filter(|e| e.is_interactive()).rev()
    }

    /// All interactive elements' ids in hit-priority order.
    pub fn interactive_ids(&self) -> Vec<ElementId> {
        self.hit_priority().map(|e| e.id).collect()
    }
}

/// The Phase 3a prototype character: a cube with a face.
///
/// Front is +Z. The body is the classic coral PoC cube; the cream face
/// plate, eyes and mouth make the front direction unmistakable; the two
/// accent-colored arms read as decorative side elements. Everything is
/// data — renderers draw whatever model they are handed.
pub fn prototype_cube() -> CharacterModel {
    let root = ModelElement {
        id: ElementId(0),
        role: ElementRole::Root,
        name: "root".into(),
        parent: None,
        offset: [0.0, 0.0, 0.0],
        parts: Vec::new(),
        hit_extent: None,
    };
    let body = ModelElement {
        id: ElementId(1),
        role: ElementRole::Body,
        name: "body".into(),
        parent: Some(ElementId(0)),
        offset: [0.0, 0.0, 0.0],
        parts: vec![VisualPart {
            offset: [0.0, 0.0, 0.0],
            size: [1.0, 1.0, 1.0],
            color: [0.94, 0.38, 0.28],
        }],
        // Half-extent 0.5 -> the grab rect the Phase 2 PoC used
        // (0.5 * sqrt(2) * 1.12 of the character height in px).
        hit_extent: Some([0.5, 0.5]),
    };
    let left_arm = ModelElement {
        id: ElementId(2),
        role: ElementRole::LeftArm,
        name: "left_arm".into(),
        parent: Some(ElementId(1)),
        offset: [-0.585, -0.02, 0.0],
        parts: vec![VisualPart {
            offset: [0.0, 0.0, 0.0],
            size: [0.13, 0.40, 0.13],
            color: [0.98, 0.80, 0.30],
        }],
        hit_extent: Some([0.08, 0.21]),
    };
    let right_arm = ModelElement {
        id: ElementId(3),
        role: ElementRole::RightArm,
        name: "right_arm".into(),
        parent: Some(ElementId(1)),
        offset: [0.585, -0.02, 0.0],
        parts: vec![VisualPart {
            offset: [0.0, 0.0, 0.0],
            size: [0.13, 0.40, 0.13],
            color: [0.98, 0.80, 0.30],
        }],
        hit_extent: Some([0.08, 0.21]),
    };
    let face = ModelElement {
        id: ElementId(4),
        role: ElementRole::Head,
        name: "face".into(),
        parent: Some(ElementId(1)),
        offset: [0.0, 0.02, 0.5],
        parts: vec![
            VisualPart {
                offset: [0.0, 0.0, 0.0],
                size: [0.66, 0.66, 0.06],
                color: [0.99, 0.93, 0.82],
            },
            VisualPart {
                offset: [0.0, -0.13, 0.045],
                size: [0.20, 0.05, 0.04],
                color: [0.47, 0.25, 0.20],
            },
        ],
        hit_extent: Some([0.36, 0.36]),
    };
    let eyes = ModelElement {
        id: ElementId(5),
        role: ElementRole::Custom("eyes".into()),
        name: "eyes".into(),
        parent: Some(ElementId(4)),
        offset: [0.0, 0.14, 0.53],
        parts: vec![
            VisualPart {
                offset: [-0.14, 0.0, 0.0],
                size: [0.10, 0.15, 0.05],
                color: [0.13, 0.11, 0.10],
            },
            VisualPart {
                offset: [0.14, 0.0, 0.0],
                size: [0.10, 0.15, 0.05],
                color: [0.13, 0.11, 0.10],
            },
        ],
        // Deliberately compact: the eyes sit above the anchor, and their
        // hit rect must not swallow the anchor point (which belongs to
        // the face element below them).
        hit_extent: Some([0.16, 0.07]),
    };
    CharacterModel::new(vec![root, body, left_arm, right_arm, face, eyes])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prototype_cube_exposes_the_documented_elements() {
        let model = prototype_cube();
        let roles: Vec<&ElementRole> = model.elements().iter().map(|e| &e.role).collect();
        assert!(roles.contains(&&ElementRole::Root));
        assert!(roles.contains(&&ElementRole::Body));
        assert!(roles.contains(&&ElementRole::Head));
        assert!(roles.contains(&&ElementRole::LeftArm));
        assert!(roles.contains(&&ElementRole::RightArm));
        assert!(roles.contains(&&ElementRole::Custom("eyes".into())));
    }

    #[test]
    fn root_is_first_and_not_interactive() {
        let model = prototype_cube();
        let root = model.elements().first().unwrap();
        assert_eq!(root.role, ElementRole::Root);
        assert!(!root.is_interactive());
        assert!(root.parts.is_empty());
    }

    #[test]
    fn parents_precede_children_in_render_order() {
        let model = prototype_cube();
        for element in model.elements() {
            if let Some(parent) = element.parent {
                assert!(
                    parent.0 < element.id.0,
                    "{} must be stored before {}",
                    parent,
                    element.id
                );
            }
        }
    }

    #[test]
    fn hit_priority_lists_children_before_parents() {
        let model = prototype_cube();
        let priority: Vec<ElementId> = model.interactive_ids();
        // Last stored interactive element (eyes) first, body last.
        assert_eq!(priority.first().copied(), Some(ElementId(5)));
        assert_eq!(priority.last().copied(), Some(ElementId(1)));
        // Root is never interactive.
        assert!(!priority.contains(&ElementId(0)));
    }

    #[test]
    fn every_visual_part_has_positive_extent() {
        for element in prototype_cube().elements() {
            for part in &element.parts {
                assert!(part.size.iter().all(|&s| s > 0.0), "{}", element.name);
            }
        }
    }

    #[test]
    fn every_element_resolves_through_its_id() {
        let model = prototype_cube();
        for (index, element) in model.elements().iter().enumerate() {
            assert_eq!(model.element(ElementId(index as u32)).name, element.name);
        }
    }
}
