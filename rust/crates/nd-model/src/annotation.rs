//! Annotation DTO — port of `models/annotation.go:8` (`type Annotations struct`).
//!
//! Port decisions (shared with the rest of #06, do not diverge):
//!   * Go `*time.Time` → `Option<String>` of ISO-8601 text: these DTOs are
//!     transforms-only, timestamp parsing belongs to the DB lane (#05).
//!   * Every Go field carries `omitempty`, so serde's `skip_serializing_if`
//!     (the house spelling, `nd-conf/src/config.rs:57`) reproduces Go's wire.

use crate::wire::{is_false, is_zero_f64, is_zero_i32, is_zero_i64};
use serde::{Deserialize, Serialize};

/// Go's `Annotations` struct (`models/annotation.go:8`): the per-user state
/// mixed into the Album/Artist/MediaFile DTOs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Annotation {
    #[serde(rename = "playCount", skip_serializing_if = "is_zero_i64")]
    /// Times the item was played.
    pub play_count: i64,

    #[serde(rename = "playDate", skip_serializing_if = "Option::is_none")]
    /// When the item was last played.
    pub play_date: Option<String>,

    #[serde(rename = "rating", skip_serializing_if = "is_zero_i32")]
    /// The user's own star rating.
    pub rating: i32,

    #[serde(rename = "ratedAt", skip_serializing_if = "Option::is_none")]
    /// When the rating was last set.
    pub rated_at: Option<String>,

    #[serde(rename = "starred", skip_serializing_if = "is_false")]
    /// Whether the item is starred.
    pub starred: bool,

    #[serde(rename = "starredAt", skip_serializing_if = "Option::is_none")]
    /// When the item was starred.
    pub starred_at: Option<String>,

    #[serde(rename = "averageRating", skip_serializing_if = "is_zero_f64")]
    /// The average rating across users.
    pub average_rating: f64,
}

/// The zero `Annotations` value (Go's default-constructed struct).
pub fn empty_annotation() -> Annotation {
    Annotation::default()
}

#[cfg(test)]
mod annotation_test {
    use super::*;
    use serde_json::to_string;

    #[test]
    fn empty_annotation_marshals_to_an_empty_object() {
        // Arrange / Act
        let wire = to_string(&empty_annotation()).unwrap_or_else(|_| "{}".to_string());
        // Assert
        assert_eq!(
            wire, "{}",
            "Go emits an empty JSON object for a zero Annotations"
        );
    }
}
