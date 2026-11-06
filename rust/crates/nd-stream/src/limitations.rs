//! Port of `core/stream/limitations.go` (171 LOC): codec-profile limitation
//! checks (`checkLimitations`) and adjustments (`applyLimitation`).

use crate::aliases::contains_ignore_case;
use crate::types::{
    Limitation, StreamDetails, COMPARISON_EQUALS, COMPARISON_GREATER_THAN_EQUAL,
    COMPARISON_LESS_THAN_EQUAL, COMPARISON_NOT_EQUALS, LIMITATION_AUDIO_BITDEPTH,
    LIMITATION_AUDIO_BITRATE, LIMITATION_AUDIO_CHANNELS, LIMITATION_AUDIO_PROFILE,
    LIMITATION_AUDIO_SAMPLERATE,
};

/// `adjustResult` (`limitations.go:9-15`): the outcome of applying a
/// limitation to a transcoded stream value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdjustResult {
    /// `adjustNone` — value already satisfies the limitation.
    None,
    /// `adjustAdjusted` — value was changed to fit the limitation.
    Adjusted,
    /// `adjustCannotFit` — cannot satisfy the limitation (reject this
    /// profile).
    CannotFit,
}

/// `checkLimitations` (`limitations.go:19-49`): checks codec profile
/// limitations against source stream details. Returns `None` if all
/// limitations pass, or the typed reason string for the first failure.
#[must_use]
pub fn check_limitations(src: &StreamDetails, limitations: &[Limitation]) -> Option<String> {
    for lim in limitations {
        let (ok, reason) = match lim.name.as_str() {
            LIMITATION_AUDIO_CHANNELS => (
                check_int_limitation(src.channels, &lim.comparison, &lim.values),
                "audio channels not supported",
            ),
            LIMITATION_AUDIO_SAMPLERATE => (
                check_int_limitation(src.sample_rate, &lim.comparison, &lim.values),
                "audio samplerate not supported",
            ),
            LIMITATION_AUDIO_BITRATE => (
                check_int_limitation(src.bitrate, &lim.comparison, &lim.values),
                "audio bitrate not supported",
            ),
            LIMITATION_AUDIO_BITDEPTH => (
                check_int_limitation(src.bit_depth, &lim.comparison, &lim.values),
                "audio bitdepth not supported",
            ),
            LIMITATION_AUDIO_PROFILE => (
                check_string_limitation(&src.profile, &lim.comparison, &lim.values),
                "audio profile not supported",
            ),
            _ => continue,
        };
        if !ok && lim.required {
            return Some(reason.to_string());
        }
    }
    None
}

/// `applyLimitation` (`limitations.go:53-73`): adjusts a transcoded stream
/// parameter to satisfy the limitation. Returns the adjustment result.
pub fn apply_limitation(
    source_bitrate: i64,
    lim: &Limitation,
    ts: &mut StreamDetails,
) -> AdjustResult {
    match lim.name.as_str() {
        LIMITATION_AUDIO_CHANNELS => {
            let current = ts.channels;
            apply_int_limitation(&lim.comparison, &lim.values, current, &mut |v| {
                ts.channels = v
            })
        }
        LIMITATION_AUDIO_BITRATE => {
            let current = if ts.bitrate == 0 {
                source_bitrate
            } else {
                ts.bitrate
            };
            apply_int_limitation(&lim.comparison, &lim.values, current, &mut |v| {
                ts.bitrate = v
            })
        }
        LIMITATION_AUDIO_SAMPLERATE => {
            let current = ts.sample_rate;
            apply_int_limitation(&lim.comparison, &lim.values, current, &mut |v| {
                ts.sample_rate = v
            })
        }
        LIMITATION_AUDIO_BITDEPTH => {
            if ts.bit_depth > 0 {
                let current = ts.bit_depth;
                apply_int_limitation(&lim.comparison, &lim.values, current, &mut |v| {
                    ts.bit_depth = v
                })
            } else {
                AdjustResult::None
            }
        }
        // LIMITATION_AUDIO_PROFILE: TODO in Go too — implement when audio
        // profile data is available (`limitations.go:69-71`).
        _ => AdjustResult::None,
    }
}

/// `applyIntLimitation` (`limitations.go:77-136`): applies a limitation
/// comparison to a value. If the value needs adjusting, calls the setter and
/// returns the result.
pub fn apply_int_limitation(
    comparison: &str,
    values: &[String],
    current: i64,
    setter: &mut dyn FnMut(i64),
) -> AdjustResult {
    if values.is_empty() {
        return AdjustResult::None;
    }
    match comparison {
        COMPARISON_LESS_THAN_EQUAL => {
            let Some(limit) = parse_int(&values[0]) else {
                return AdjustResult::None;
            };
            if current <= limit {
                return AdjustResult::None;
            }
            setter(limit);
            AdjustResult::Adjusted
        }
        COMPARISON_GREATER_THAN_EQUAL => {
            let Some(limit) = parse_int(&values[0]) else {
                return AdjustResult::None;
            };
            if current >= limit {
                return AdjustResult::None;
            }
            // Cannot upscale
            AdjustResult::CannotFit
        }
        COMPARISON_EQUALS => {
            // Check if current value matches any allowed value
            for v in values {
                if let Some(limit) = parse_int(v) {
                    if current == limit {
                        return AdjustResult::None;
                    }
                }
            }
            // Find the closest allowed value below current (don't upscale)
            let mut closest: i64 = 0;
            let mut found = false;
            for v in values {
                if let Some(limit) = parse_int(v) {
                    if limit < current && (!found || limit > closest) {
                        closest = limit;
                        found = true;
                    }
                }
            }
            if found {
                setter(closest);
                return AdjustResult::Adjusted;
            }
            AdjustResult::CannotFit
        }
        COMPARISON_NOT_EQUALS => {
            for v in values {
                if let Some(limit) = parse_int(v) {
                    if current == limit {
                        return AdjustResult::CannotFit;
                    }
                }
            }
            AdjustResult::None
        }
        _ => AdjustResult::None,
    }
}

/// `checkIntLimitation` (`limitations.go:138-140`): a check-shaped wrapper
/// around `apply_int_limitation` with a no-op setter — `None` means pass.
#[must_use]
pub fn check_int_limitation(value: i64, comparison: &str, values: &[String]) -> bool {
    apply_int_limitation(comparison, values, value, &mut |_| {}) == AdjustResult::None
}

/// `checkStringLimitation` (`limitations.go:145-163`): checks a string value
/// against a limitation. Only Equals and NotEquals comparisons are meaningful
/// for strings; LessThanEqual/GreaterThanEqual are not applicable and always
/// pass.
#[must_use]
pub fn check_string_limitation(value: &str, comparison: &str, values: &[String]) -> bool {
    match comparison {
        COMPARISON_EQUALS => contains_ignore_case(values, value),
        COMPARISON_NOT_EQUALS => !contains_ignore_case(values, value),
        _ => true,
    }
}

/// `parseInt` (`limitations.go:165-171`): `strconv.Atoi` rejecting negatives
/// and junk.
fn parse_int(s: &str) -> Option<i64> {
    match s.parse::<i64>() {
        Ok(v) if v >= 0 => Some(v),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lim(name: &str, comparison: &str, values: &[&str], required: bool) -> Limitation {
        Limitation {
            name: name.to_string(),
            comparison: comparison.to_string(),
            values: values.iter().map(|v| (*v).to_string()).collect(),
            required,
        }
    }

    #[test]
    fn check_int_limitation_less_than_equal() {
        // Act/Assert
        assert!(check_int_limitation(
            96,
            COMPARISON_LESS_THAN_EQUAL,
            &["320".to_string()]
        ));
        assert!(!check_int_limitation(
            512,
            COMPARISON_LESS_THAN_EQUAL,
            &["320".to_string()]
        ));
    }

    #[test]
    fn check_int_limitation_equals_multiple_values() {
        // Act/Assert (decider_test.go "handles Equals comparison with multiple values")
        assert!(check_int_limitation(
            2,
            COMPARISON_EQUALS,
            &["1".to_string(), "2".to_string()]
        ));
        assert!(!check_int_limitation(
            6,
            COMPARISON_EQUALS,
            &["1".to_string(), "2".to_string()]
        ));
    }

    #[test]
    fn check_int_limitation_greater_than_equal_cannot_upscale() {
        // Arrange/Act/Assert (decider_test.go "rejects transcoding profile when
        // GreaterThanEqual cannot be satisfied")
        assert!(check_int_limitation(
            96_000,
            COMPARISON_GREATER_THAN_EQUAL,
            &["96000".to_string()]
        ));
        assert!(!check_int_limitation(
            44_100,
            COMPARISON_GREATER_THAN_EQUAL,
            &["96000".to_string()]
        ));
    }

    #[test]
    fn check_int_limitation_not_equals() {
        // Act/Assert
        assert!(check_int_limitation(
            2,
            COMPARISON_NOT_EQUALS,
            &["6".to_string()]
        ));
        assert!(!check_int_limitation(
            6,
            COMPARISON_NOT_EQUALS,
            &["6".to_string()]
        ));
    }

    #[test]
    fn apply_int_limitation_downscales_to_limit() {
        // Arrange
        let l = lim(
            LIMITATION_AUDIO_BITRATE,
            COMPARISON_LESS_THAN_EQUAL,
            &["96"],
            true,
        );
        let mut ts = StreamDetails {
            bitrate: 192,
            ..StreamDetails::default()
        };
        // Act
        let result = apply_limitation(0, &l, &mut ts);
        // Assert
        assert_eq!(result, AdjustResult::Adjusted);
        assert_eq!(ts.bitrate, 96);
    }

    #[test]
    fn apply_int_limitation_bitrate_falls_back_to_source_when_zero() {
        // Arrange (`limitations.go:57-62`)
        let l = lim(
            LIMITATION_AUDIO_BITRATE,
            COMPARISON_LESS_THAN_EQUAL,
            &["128"],
            true,
        );
        let mut ts = StreamDetails::default();
        // Act
        let result = apply_limitation(320, &l, &mut ts);
        // Assert
        assert_eq!(result, AdjustResult::Adjusted);
        assert_eq!(ts.bitrate, 128);
    }

    #[test]
    fn apply_int_limitation_equals_uses_closest_below_current() {
        // Arrange (`limitations.go:103-125`)
        let l = lim(
            LIMITATION_AUDIO_CHANNELS,
            COMPARISON_EQUALS,
            &["1", "2"],
            true,
        );
        let mut ts = StreamDetails {
            channels: 6,
            ..StreamDetails::default()
        };
        // Act
        let result = apply_limitation(0, &l, &mut ts);
        // Assert: closest below 6 is 2
        assert_eq!(result, AdjustResult::Adjusted);
        assert_eq!(ts.channels, 2);
    }

    #[test]
    fn apply_int_limitation_bitdepth_skipped_when_zero() {
        // Arrange (`limitations.go:65-68`)
        let l = lim(
            LIMITATION_AUDIO_BITDEPTH,
            COMPARISON_LESS_THAN_EQUAL,
            &["16"],
            true,
        );
        let mut ts = StreamDetails::default();
        // Act
        let result = apply_limitation(0, &l, &mut ts);
        // Assert
        assert_eq!(result, AdjustResult::None);
        assert_eq!(ts.bit_depth, 0);
    }

    #[test]
    fn apply_int_limitation_junk_values_are_noop() {
        // Arrange: `parseInt` rejects non-numerics (`limitations.go:165-171`)
        let l = lim(
            LIMITATION_AUDIO_SAMPLERATE,
            COMPARISON_LESS_THAN_EQUAL,
            &["N/A"],
            true,
        );
        let mut ts = StreamDetails {
            sample_rate: 96_000,
            ..StreamDetails::default()
        };
        // Act/Assert
        assert_eq!(apply_limitation(0, &l, &mut ts), AdjustResult::None);
        assert_eq!(ts.sample_rate, 96_000);
    }

    #[test]
    fn check_string_limitation_equals_case_insensitive() {
        // Act/Assert (`limitations.go:145-163`)
        assert!(check_string_limitation(
            "LC",
            COMPARISON_EQUALS,
            &["lc".to_string()]
        ));
        assert!(!check_string_limitation(
            "HE-AAC",
            COMPARISON_EQUALS,
            &["lc".to_string()]
        ));
        assert!(check_string_limitation(
            "LC",
            COMPARISON_NOT_EQUALS,
            &["he-aac".to_string()]
        ));
        assert!(!check_string_limitation(
            "LC",
            COMPARISON_NOT_EQUALS,
            &["lc".to_string()]
        ));
        // Non-equality comparisons always pass.
        assert!(check_string_limitation(
            "LC",
            COMPARISON_LESS_THAN_EQUAL,
            &["lc".to_string()]
        ));
    }
}
