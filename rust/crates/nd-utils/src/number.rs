//! Numeric helpers — port of `utils/number`.

/// Conversion from `i64` with Go's `T(r)` cast-style truncation (wrapping).
pub trait FromI64: Sized {
    /// Converts from `i64`, wrapping like Go's integer conversion.
    fn from_i64(v: i64) -> Self;
}

macro_rules! impl_from_i64 {
    ($($t:ty),*) => {
        $(impl FromI64 for $t {
            fn from_i64(v: i64) -> Self {
                v as $t
            }
        })*
    };
}

impl_from_i64!(i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, u128, usize);

/// Parses `s` as a base-10 integer into `T`, returning the zero value on
/// failure (Go `number.ParseInt`). Values beyond the target type wrap like
/// Go's `T(r)` conversion.
#[must_use]
pub fn parse_int<T: Default + FromI64>(s: &str) -> T {
    let r: i64 = s.parse().unwrap_or(0);
    if r == 0 {
        return T::default();
    }
    T::from_i64(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_into_each_integer_type() {
        assert_eq!(parse_int::<i64>("123"), 123_i64);
        assert_eq!(parse_int::<i32>("123"), 123_i32);
        assert_eq!(parse_int::<i128>("123"), 123_i128);
        assert_eq!(parse_int::<u64>("123"), 123_u64);
        assert_eq!(parse_int::<usize>("123"), 123_usize);
    }

    #[test]
    fn returns_zero_on_parse_failure() {
        assert_eq!(parse_int::<i64>("nope"), 0);
        assert_eq!(parse_int::<i64>(""), 0);
        assert_eq!(parse_int::<u32>(""), 0);
    }

    #[test]
    fn wraps_like_a_go_cast() {
        // Go: int32(int64(4294967296)) wraps to 0.
        assert_eq!(parse_int::<i32>("4294967296"), 0);
    }
}
