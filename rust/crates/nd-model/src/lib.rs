// nd-model: owned by #06 (docs/rust-port/issues.md Part B)

//! Scaffold stage for nd-model: this crate instantiates empty so later issues
//! (#06) can fill it without pulling the world in (#S1 test contract).

/// Scaffold marker: the public surface authored by #06 replaces this constant.
pub const OWNED_BY_ISSUE: u8 = 6;

#[cfg(test)]
mod scaffold_tests {
    use super::*;

    #[test]
    fn nd_model_instantiates() {
        // Arrange: the owning-issue number this crate must expose.
        let expected: u8 = 6;
        // Act: read the crate's own scaffold marker.
        let actual: u8 = OWNED_BY_ISSUE;
        // Assert
        assert_eq!(
            actual, expected,
            "nd-model instantiates without pulling in the world"
        );
    }
}
pub mod album;
pub mod annotation;
pub mod criteria;
pub mod entity;
pub mod fields;
pub mod json;
pub mod operators;
pub mod sort;
pub mod wire;

// Criteria DSL re-exports (criteria.go / json.go).
pub use crate::criteria::child_playlist_ids;
pub use crate::criteria::child_playlist_paths;
pub use crate::criteria::effective_limit;
pub use crate::criteria::is_percentage_limit;
pub use crate::criteria::marshal_criteria;
pub use crate::criteria::resolve_limit;
pub use crate::criteria::unmarshal_criteria;
pub use crate::criteria::Criteria;
pub use crate::criteria::CriteriaError;
pub use crate::json::to_bool;
pub use crate::json::OptionalConjunction;

pub use crate::fields::add_numeric_tags;
pub use crate::fields::add_roles;
pub use crate::fields::add_tag_names;
pub use crate::fields::all_field_names;
pub use crate::fields::default_field_map;
pub use crate::fields::lookup_field;
pub use crate::fields::FieldInfo;
pub use crate::fields::FieldMap;

pub use crate::operators::expression_key;
pub use crate::operators::extract_playlist_field;
pub use crate::operators::fields;
pub use crate::operators::walk;
pub use crate::operators::Expression;
pub use crate::operators::Fields;

pub use crate::sort::SortField;
