//! Sort-field transforms — port of `model/criteria/sort.go` (63 LOC).
//!
//! Go logs rejected fields through `log.Error`/`log.Warn` (`sort.go:22`,
//! `sort.go:40`, `sort.go:49`); the port is a pure transform and simply drops
//! the invalid part, keeping the same fallbacks (`sort.go:50` returns the
//! `title` ascending default when nothing valid remains).

use crate::fields::lookup_field;
use crate::fields::FieldMap;

/// Go `SortField` (`model/criteria/sort.go:9`).
#[derive(Debug, Clone, PartialEq)]
pub struct SortField {
    /// Canonical field name — Go `SortField.Field` (`sort.go:11`).
    pub field: String,
    /// Sort descending — Go `SortField.Desc` (`sort.go:12`).
    pub desc: bool,
}

/// The fallback sort value — Go `sort.go:16`/`:17` and the fallback of `sort.go:50`.
const DEFAULT_SORT: &str = "title";

/// Whether `order` is a usable direction — Go `sort.go:21`.
#[must_use]
pub fn is_valid_order(order: String) -> bool {
    order == "asc" || order == "desc"
}

/// Go `Criteria.OrderByFields()` (`model/criteria/sort.go:14`): split the raw
/// `sort` string, honour `+`/`-` prefixes, resolve each part through the field
/// registry, and invert every direction when `order` is `desc`.
pub fn order_by_fields(map: &FieldMap, sort: String, order: String) -> Vec<SortField> {
    let mut sort_value: String = sort;
    if sort_value.is_empty() {
        sort_value = DEFAULT_SORT.to_string();
    }

    // Go `sort.go:20-24`: bad `order` values are demoted to "no direction".
    let mut normalized_order: String = order.to_lowercase();
    if !normalized_order.is_empty() && !is_valid_order(normalized_order.to_string()) {
        normalized_order = String::new();
    }

    let mut fields: Vec<SortField> = Vec::new();
    for part in sort_value.split(",") {
        let mut trimmed: String = part.trim().to_string();
        if trimmed.is_empty() {
            continue;
        }

        // Go `sort.go:34-37`: a leading '+' or '-' sets the direction, not the field.
        let mut desc: bool = false;
        if trimmed.starts_with("+") || trimmed.starts_with("-") {
            desc = trimmed.starts_with("-");
            trimmed = trimmed[1..].trim().to_string();
        }

        // Go `sort.go:38-42`: unknown fields are dropped.
        let (_info, canonical, ok) = lookup_field(map, trimmed);
        if !ok {
            continue;
        }

        // Go `sort.go:43-45`.
        if normalized_order == "desc" {
            desc = !desc;
        }

        fields.push(SortField {
            field: canonical,
            desc,
        });
    }

    // Go `sort.go:48-51`.
    if fields.is_empty() {
        return vec![SortField {
            field: DEFAULT_SORT.to_string(),
            desc: false,
        }];
    }
    fields
}

/// Go `Criteria.SortFieldNames()` (`model/criteria/sort.go:55`).
pub fn sort_field_names(map: &FieldMap, sort: String, order: String) -> Vec<String> {
    let sort_fields: Vec<SortField> = order_by_fields(map, sort, order);
    let mut names: Vec<String> = Vec::new();
    for sf in &sort_fields {
        names.push(sf.field.to_string());
    }
    names
}

#[cfg(test)]
mod sort_test {
    use super::*;
    use crate::fields::default_field_map;

    #[test]
    fn sort_field_names_both_order_directions() {
        // Arrange: port of `model/criteria/sort_test.go:37-43` (desc inverts
        // every direction) and `:71-74` (asc keeps the prefixes as written).
        let map = default_field_map();
        // Act:
        let asc = order_by_fields(&map, "title,-rating".to_string(), "asc".to_string());
        let desc = order_by_fields(&map, "title,-rating".to_string(), "desc".to_string());
        let names = sort_field_names(&map, "title,-rating".to_string(), "desc".to_string());
        // Assert: ascending branch keeps the prefixes (`sort.go:33`-`:36`).
        assert_eq!(asc.len(), 2, "sort.go:26 — both parts are valid fields");
        assert_eq!(asc[0].field, "title", "sort.go:46 — canonical field name");
        assert!(!asc[0].desc, "sort.go:33 — no prefix means ascending");
        assert!(asc[1].desc, "sort.go:35 — '-' prefix means descending");
        // Assert: descending branch inverts them (`sort.go:43`-`:45`).
        assert_eq!(desc.len(), 2, "sort.go:26");
        assert!(desc[0].desc, "sort.go:44 — asc inverted to desc");
        assert!(!desc[1].desc, "sort.go:44 — desc inverted back to asc");
        // Assert: `SortFieldNames` carries the same directions (`sort.go:55`).
        assert_eq!(names.len(), 2, "sort_test.go:96 — one name per sort field");
        assert_eq!(names[0], "title", "sort.go:46 — alias-free canonical names");
    }
}
