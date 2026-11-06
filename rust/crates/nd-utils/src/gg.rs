//! Simple Go-language "extensions" — port of `utils/gg` (based on
//! <https://github.com/icza/gog>). Pointer helpers map to Rust `Option`.

/// Returns the value of `p`, or `None` if `p` is `None` (Go `gg.V` returns the
/// zero value; Rust `Option` is the honest equivalent).
#[must_use]
pub fn v<T>(p: Option<T>) -> Option<T> {
    p
}

/// Returns `v1` if `cond` is true, `v2` otherwise (Go `gg.If`).
#[must_use]
pub fn if_<T>(cond: bool, v1: T, v2: T) -> T {
    if cond {
        v1
    } else {
        v2
    }
}

/// Returns a fresh copy of `p` (Go `gg.Clone`). Rust's `Clone` semantics make
/// this a plain clone of the boxed value.
#[must_use]
pub fn clone<T: Clone>(p: Option<Box<T>>) -> Option<Box<T>> {
    p.map(|b| Box::new(*b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v_unwraps_a_present_pointer() {
        assert_eq!(v(Some(123)), Some(123));
        assert_eq!(v(None::<i32>), None);
    }

    #[test]
    fn if_returns_the_selected_branch() {
        assert_eq!(if_(true, 1, 2), 1);
        assert_eq!(if_(false, 1, 2), 2);
        assert_eq!(if_(true, "a", "b"), "a");
        assert_eq!(if_(false, "a", "b"), "b");
        assert_eq!(if_(true, 1.1, 2.2), 1.1);
        assert_eq!(if_(false, 1.1, 2.2), 2.2);
    }

    #[test]
    fn clone_does_not_alias_the_original() {
        let original = 123_i32;
        let cloned = clone(Some(Box::new(original)));
        assert_eq!(cloned.map(|b| *b), Some(123));
        assert_eq!(clone::<i32>(None), None);
    }
}
