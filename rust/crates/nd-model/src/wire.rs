//! `skip_serializing_if` predicates — the port's spelling of Go's
//! `json:"…,omitempty"` tag (house precedent: `nd-conf/src/config.rs:57`).
//!
//! serde needs a concrete predicate per field type, and it calls the predicate
//! with a reference to the field, so the ladder is one tiny fn per Go scalar
//! kind rather than a generic.

/// Go `omitempty` for an `int`/`int64`.
pub fn is_zero_i32(value: &i32) -> bool {
    *value == 0
}

/// Go `omitempty` for an `int64`.
pub fn is_zero_i64(value: &i64) -> bool {
    *value == 0
}

/// Go `omitempty` for a `float32`/`float64`.
pub fn is_zero_f64(value: &f64) -> bool {
    *value == 0.0
}

/// Go `omitempty` for a `bool`.
pub fn is_false(value: &bool) -> bool {
    !*value
}

/// Go `omitempty` for a `[]T`/`map[K]V`.
pub fn is_empty_strings(values: &[String]) -> bool {
    values.is_empty()
}

#[cfg(test)]
mod wire_test {
    use super::*;

    #[test]
    fn go_omitempty_predicates() {
        // Arrange / Act
        let (zero_i, zero_f, false_b, empty_v) = (
            is_zero_i32(&0),
            is_zero_f64(&0.0),
            is_false(&false),
            is_empty_strings([].as_ref()),
        );
        // Assert
        assert!(zero_i, "Go omits a zero int");
        assert!(zero_f, "Go omits a zero float");
        assert!(false_b, "Go omits a false bool");
        assert!(empty_v, "Go omits an empty slice");
    }
}
