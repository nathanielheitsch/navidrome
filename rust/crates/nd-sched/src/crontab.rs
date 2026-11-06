//! Port of `scheduler/crontab_schedule.go` on top of the robfig/cron parser
//! semantics (parser.go/spec.go of robfig v3.0.1):
//!
//! * duration strings (`"5m"`) become `@every 5m`;
//! * `@`-descriptors (`@hourly`, `@daily`, `@weekly`, `@monthly`, `@yearly`,
//!   `@every <duration>`, `@midnight`, `@annually`);
//! * `TZ=`/`CRON_TZ=` location prefixes;
//! * 5-field (minute-first) and 6-field (seconds-first) expressions, with
//!   `*`/`?`, ranges `a-b`, steps `x/y` (and `*/y`), lists, and month/day
//!   names;
//! * the crontab(5) random `~` syntax, resolved once at parse time to a
//!   random value inside the field bounds (crontab_schedule.go:88-122).

use rand::Rng;

use crate::schedule::{every, Schedule, SpecSchedule, STAR_BIT};

/// Error returned by [`parse_crontab`], mirroring the Go messages.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct CrontabError {
    /// Human-readable reason (Go error strings carry no structured fields).
    pub message: String,
}

fn err<T>(msg: String) -> Result<T, CrontabError> {
    Err(CrontabError { message: msg })
}

/// Parses a cron expression with support for the crontab(5) random `~`
/// syntax (port of `scheduler.ParseCrontab`, crontab_schedule.go:20-42).
/// Random values are resolved once at parse time. Duration strings (e.g.
/// `"5m"`) are converted to `@every 5m`. Config values `"0"` and `""` (a
/// disabled schedule, conf/configuration.go:885) are parsed as `@every 0s`
/// for validator-agreement parity.
pub fn parse_crontab(input: &str) -> Result<Schedule, CrontabError> {
    let owned;
    let mut spec = input;
    if spec.is_empty() {
        return err("empty spec string".to_string());
    }
    if spec == "0" || nd_conf::duration::parse_go_duration(spec).is_ok() {
        owned = format!("@every {spec}");
        spec = &owned;
    }

    if !spec.contains('~') {
        return parse_robfig(spec);
    }

    // Handle TZ=/CRON_TZ= prefix (crontab_schedule.go:33-42).
    let mut tz_prefix = "";
    if spec.starts_with("TZ=") || spec.starts_with("CRON_TZ=") {
        let Some(i) = spec.find(' ') else {
            return err("missing spec after timezone".to_string());
        };
        tz_prefix = &spec[..i + 1];
        spec = spec[i..].trim_start();
    }

    // @ descriptors cannot contain ~.
    if spec.starts_with('@') {
        return err(format!(
            "random ~ syntax cannot be used with descriptors: {spec}"
        ));
    }

    let mut fields: Vec<String> = spec.split_whitespace().map(String::from).collect();
    fields = normalize_fields(fields)?;

    // Resolve each ~ field to a concrete random value
    // (crontab_schedule.go:66-81).
    for (i, field) in fields.iter_mut().enumerate() {
        if !field.contains('~') {
            continue;
        }
        if field.contains(',') || field.contains('/') {
            return err(format!(
                "random ~ cannot be combined with lists or steps: {field}"
            ));
        }
        let v = resolve_random_field(field, FIELD_BOUNDS[i])?;
        *field = v.to_string();
    }

    // Re-assemble and parse (crontab_schedule.go:124-126).
    let resolved = format!("{tz_prefix}{}", fields.join(" "));
    parse_robfig(&resolved)
}

/// Port of `normalizeFields` (crontab_schedule.go:114-122): 5 fields get a
/// `0` second prepended; 6 stay as-is.
fn normalize_fields(fields: Vec<String>) -> Result<Vec<String>, CrontabError> {
    match fields.len() {
        5 => {
            let mut with_second = vec![String::from("0")];
            with_second.extend(fields);
            Ok(with_second)
        }
        6 => Ok(fields),
        n => err(format!("expected 5 or 6 fields, found {n}")),
    }
}

/// Inclusive bounds of each cron field, matching the Go `fieldBounds` table
/// (crontab_schedule.go:47-52) and robfig's `bounds`.
const FIELD_BOUNDS: [(u32, u32); 6] = [
    (0, 59), // Second
    (0, 59), // Minute
    (0, 23), // Hour
    (1, 31), // Dom
    (1, 12), // Month
    (0, 6),  // Dow
];

/// Month names (robfig parser.go months map).
const MONTH_NAMES: [(&str, u32); 12] = [
    ("jan", 1),
    ("feb", 2),
    ("mar", 3),
    ("apr", 4),
    ("may", 5),
    ("jun", 6),
    ("jul", 7),
    ("aug", 8),
    ("sep", 9),
    ("oct", 10),
    ("nov", 11),
    ("dec", 12),
];

/// Second names (robfig parser.go seconds map).
/// Weekday names (robfig parser.go dow map).
const DOW_NAMES: [(&str, u32); 7] = [
    ("sun", 0),
    ("mon", 1),
    ("tue", 2),
    ("wed", 3),
    ("thu", 4),
    ("fri", 5),
    ("sat", 6),
];

/// Port of `resolveRandomField` (crontab_schedule.go:88-110): `~` is
/// `min~max`; `A~` is `A~field-max`, `~B` is `field-min~B`, bare `~` is the
/// full field range. Returns a random value inside `[min, max]`.
fn resolve_random_field(field: &str, bounds: (u32, u32)) -> Result<u32, CrontabError> {
    let parts: Vec<&str> = field.splitn(2, '~').collect();
    let (lo_s, hi_s) = (parts[0], parts[1]);

    let mut min = bounds.0;
    let mut max = bounds.1;

    if !lo_s.is_empty() {
        min = lo_s.parse::<u32>().map_err(|_| CrontabError {
            message: format!("invalid random range start: {lo_s}"),
        })?;
    }
    if !hi_s.is_empty() {
        max = hi_s.parse::<u32>().map_err(|_| CrontabError {
            message: format!("invalid random range end: {hi_s}"),
        })?;
    }

    if min < bounds.0 {
        return err(format!(
            "random range start ({min}) below minimum ({}): {field}",
            bounds.0
        ));
    }
    if max > bounds.1 {
        return err(format!(
            "random range end ({max}) above maximum ({}): {field}",
            bounds.1
        ));
    }
    if min > max {
        return err(format!(
            "random range start ({min}) beyond end ({max}): {field}"
        ));
    }

    Ok(rand::rng().random_range(min..=max))
}

/// Port of robfig `Parser.Parse` for the option set Go uses
/// (`SecondOptional | Minute | Hour | Dom | Month | Dow | Descriptor`),
/// i.e. `normalizeFields` already happened upstream and this handles only
/// TZ prefixes, descriptors, and full 6-field specs.
fn parse_robfig(spec: &str) -> Result<Schedule, CrontabError> {
    let mut spec = spec;
    let mut location = None;

    // Extract timezone if present (parser.go:95-105).
    if spec.starts_with("TZ=") || spec.starts_with("CRON_TZ=") {
        let i = spec.find(' ').ok_or_else(|| CrontabError {
            message: "missing spec after timezone".to_string(),
        })?;
        let name = &spec[spec.find('=').unwrap() + 1..i];
        location = Some(parse_tz(name)?);
        spec = spec[i..].trim_start();
    }

    // Handle named schedules (descriptors), if configured (parser.go:108-112).
    if spec.starts_with('@') {
        return parse_descriptor(spec, location);
    }

    // Validate & fill in any omitted or optional fields (robfig
    // normalizeFields for SecondOptional: 5 fields get a `0` second).
    let mut fields: Vec<&str> = spec.split_whitespace().collect();
    match fields.len() {
        5 => fields.insert(0, "0"),
        6 => {}
        n => {
            return err(format!(
                "expected 5 or 6 fields, found {n}: [{}]",
                fields.join(", ")
            ))
        }
    }

    let second = get_field(fields[0], (0, 59), &[])?;
    let minute = get_field(fields[1], (0, 59), &[])?;
    let hour = get_field(fields[2], (0, 23), &[])?;
    let dom = get_field(fields[3], (1, 31), &[])?;
    let month = get_field(fields[4], (1, 12), &MONTH_NAMES)?;
    let dow = get_field(fields[5], (0, 6), &DOW_NAMES)?;

    Ok(Schedule::Spec(SpecSchedule {
        second,
        minute,
        hour,
        dom,
        month,
        dow,
        location,
    }))
}

fn parse_tz(name: &str) -> Result<chrono_tz::Tz, CrontabError> {
    name.parse::<chrono_tz::Tz>().map_err(|_| CrontabError {
        message: format!("provided bad location {name}"),
    })
}

/// Port of robfig `parseDescriptor` (parser.go:364-455) for the names the
/// Go validator accepts (`@every`, `@yearly`, `@annually`, `@monthly`,
/// `@weekly`, `@daily`, `@midnight`, `@hourly`).
fn parse_descriptor(
    descriptor: &str,
    location: Option<chrono_tz::Tz>,
) -> Result<Schedule, CrontabError> {
    // `all(r)` = robfig's all(): every value in bounds, plus the star bit.
    let all = |min: u32, max: u32| get_bits(min, max, 1) | STAR_BIT;
    let all_months = || all(1, 12);
    let all_dom = || all(1, 31);
    let all_dow = || all(0, 6);
    let at = |hour: u64, dom: u64, month: u64, dow: u64| {
        Schedule::Spec(SpecSchedule {
            second: 1 << 0,
            minute: 1 << 0,
            hour,
            dom,
            month,
            dow,
            location,
        })
    };
    let descriptor = &descriptor[1..]; // strip the leading '@'
    match descriptor {
        "yearly" | "annually" => Ok(at(1 << 0, 1 << 1, 1 << 1, all_dow())),
        "monthly" => Ok(at(1 << 0, 1 << 1, all_months(), all_dow())),
        "weekly" => Ok(at(1 << 0, all_dom(), all_months(), 1 << 0)),
        "daily" | "midnight" => Ok(at(1 << 0, all_dom(), all_months(), all_dow())),
        "hourly" => Ok(at(all(0, 23), all_dom(), all_months(), all_dow())),
        _ => {
            if let Some(rest) = descriptor.strip_prefix("every ") {
                let duration =
                    nd_conf::duration::parse_go_duration(rest).map_err(|e| CrontabError {
                        message: format!("failed to parse duration {rest}: {}", e.message),
                    })?;
                return Ok(Schedule::Delay(every(duration)));
            }
            err(format!("unrecognized descriptor: @{descriptor}"))
        }
    }
}

/// Port of robfig `getField` (parser.go:229-241): comma-separated ranges.
fn get_field(field: &str, bounds: (u32, u32), names: &[(&str, u32)]) -> Result<u64, CrontabError> {
    let mut bits = 0u64;
    for expr in field.split(',') {
        bits |= get_range(expr, bounds, names)?;
    }
    Ok(bits)
}

/// Port of robfig `getRange` (parser.go:247-322): `number`, `a-b`, `x/y`,
/// `a-b/y`, `*/y`, `?` (with the `N/step` = `N-max/step` special case).
fn get_range(expr: &str, bounds: (u32, u32), names: &[(&str, u32)]) -> Result<u64, CrontabError> {
    let range_and_step: Vec<&str> = expr.split('/').collect();
    let low_and_high: Vec<&str> = range_and_step[0].split('-').collect();

    let (start, mut end);
    let mut extra = 0u64;
    let single_digit = low_and_high.len() == 1;

    if low_and_high[0] == "*" || low_and_high[0] == "?" {
        start = bounds.0;
        end = bounds.1;
        extra = STAR_BIT;
    } else {
        start = parse_int_or_name(low_and_high[0], names)?;
        match low_and_high.len() {
            1 => end = start,
            2 => end = parse_int_or_name(low_and_high[1], names)?,
            _ => return err(format!("too many hyphens: {expr}")),
        }
    }

    let step = match range_and_step.len() {
        1 => 1,
        2 => {
            let s: u32 = range_and_step[1].parse().map_err(|_| CrontabError {
                message: format!("failed to parse int from {}", range_and_step[1]),
            })?;
            // Special handling: "N/step" means "N-max/step".
            if single_digit {
                end = bounds.1;
            }
            if s > 1 {
                extra = 0;
            }
            s
        }
        _ => return err(format!("too many slashes: {expr}")),
    };

    if start < bounds.0 {
        return err(format!(
            "beginning of range ({start}) below minimum ({}): {expr}",
            bounds.0
        ));
    }
    if end > bounds.1 {
        return err(format!(
            "end of range ({end}) above maximum ({}): {expr}",
            bounds.1
        ));
    }
    if start > end {
        return err(format!(
            "beginning of range ({start}) beyond end of range ({end}): {expr}"
        ));
    }
    if step == 0 {
        return err(format!("step of range should be a positive number: {expr}"));
    }

    Ok(get_bits(start, end, step) | extra)
}

fn parse_int_or_name(expr: &str, names: &[(&str, u32)]) -> Result<u32, CrontabError> {
    if !names.is_empty() {
        if let Some(&(_, named)) = names.iter().find(|(n, _)| *n == expr.to_ascii_lowercase()) {
            return Ok(named);
        }
    }
    expr.parse::<u32>().map_err(|_| CrontabError {
        message: format!("failed to parse int from {expr}"),
    })
}

/// Port of robfig `getBits` (parser.go:325-336).
fn get_bits(min: u32, max: u32, step: u32) -> u64 {
    if step == 1 {
        let upper = if max + 1 >= u64::BITS {
            0
        } else {
            u64::MAX << (max + 1)
        };
        return !upper & (u64::MAX << min);
    }
    let mut bits = 0u64;
    let mut i = min;
    while i <= max {
        bits |= 1 << i;
        i += step;
    }
    bits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::Schedule;
    use chrono::{DateTime, TimeZone, Utc};

    fn at(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, mi, s).unwrap()
    }

    /// The lowest set bit of a field bit set (ignoring the star bit), the
    /// Go test helper `findSetBit` (crontab_schedule_test.go:113-125).
    fn find_set_bit(v: u64) -> i32 {
        let v = v & !(1 << 63);
        (0..63).find(|&i| v & (1 << i) != 0).unwrap_or(-1)
    }

    fn spec(spec: &str) -> SpecSchedule {
        match parse_crontab(spec).unwrap() {
            Schedule::Spec(s) => s,
            _ => panic!("expected a spec schedule for {spec:?}"),
        }
    }

    // ===== Agreement with nd-conf's validator (the anti-drift contract) =====

    /// conf/configuration.go:905 routes the value through
    /// scheduler.ParseCrontab; nd-conf's validate_crontab is the validator
    /// port. Whatever the validator accepts (minus the `@every none` special
    /// value) must parse here, and whatever parses here (minus the `~`
    /// form the validator explicitly rejects) must pass the validator.
    #[test]
    fn agrees_with_nd_conf_validator_on_valid_strings() {
        // Arrange: values accepted by nd-conf's validate_crontab (its own
        // test list plus the config-side `"0"` disable value and names).
        // Act / Assert: every one parses here.
        for s in [
            "0",
            "5m",
            "@every 1h",
            "@daily",
            "@hourly",
            "@weekly",
            "@monthly",
            "@yearly",
            "@annually",
            "@midnight",
            "*/30 * * * *",
            "30 4 * * *",
            "* * * * * *",
            "0 0 1 1 *",
            "TZ=UTC 0 0 * * *",
            "CRON_TZ=UTC 0 0 * * *",
            "1,15 0-5 1 * *",
            "0 0 * * SUN",
            "30 5 * * * *",
        ] {
            assert!(
                nd_conf::crontab::validate_crontab(s).is_ok(),
                "validator should accept {s:?}"
            );
            assert!(
                parse_crontab(s).is_ok(),
                "parser should accept what the validator accepts: {s:?}"
            );
        }
    }

    #[test]
    fn agrees_with_nd_conf_validator_on_invalid_strings() {
        // Arrange: values rejected by nd-conf's validate_crontab.
        // Act / Assert: the parser rejects them too.
        for s in [
            "",
            "junk",
            "* * * *",
            "@bogus",
            "*/x * * * *",
            "99 * * * * *",
            "* * * * * * *",
            "@daily extra",
        ] {
            assert!(
                nd_conf::crontab::validate_crontab(s).is_err(),
                "validator should reject {s:?}"
            );
            assert!(
                parse_crontab(s).is_err(),
                "parser should reject what the validator rejects: {s:?}"
            );
        }
    }

    #[test]
    fn random_tilde_form_is_not_validator_but_parser_domain() {
        // Arrange: the documented split — the validator rejects `~` (the
        // nd-conf crontab.rs module doc pins this), the parser owns it.
        // Act / Assert:
        assert!(nd_conf::crontab::validate_crontab("0~30 * * * *").is_err());
        assert!(parse_crontab("0~30 * * * *").is_ok());
    }

    // ===== Ported literally from crontab_schedule_test.go =====

    #[test]
    fn parses_a_5_field_expression() {
        // Arrange / Act:
        let sched = parse_crontab("5 * * * *").unwrap();
        // Assert:
        assert!(matches!(sched, Schedule::Spec(_)));
    }

    #[test]
    fn parses_a_6_field_expression_with_seconds() {
        // Arrange / Act:
        let sched = parse_crontab("30 5 * * * *").unwrap();
        // Assert:
        assert!(matches!(sched, Schedule::Spec(_)));
    }

    #[test]
    fn converts_duration_string_to_every() {
        // Arrange / Act:
        let sched = parse_crontab("5m").unwrap();
        // Assert:
        assert!(matches!(sched, Schedule::Delay(_)));
    }

    #[test]
    fn returns_error_for_empty_string() {
        // Arrange / Act:
        let result = parse_crontab("");
        // Assert:
        assert!(result.is_err());
    }

    #[test]
    fn resolves_a_to_b_to_a_value_within_range() {
        // Arrange / Act:
        let s = spec("0~30 * * * *");
        let minute = find_set_bit(s.minute);
        // Assert:
        assert!(minute >= 0);
        assert!(minute <= 30);
    }

    #[test]
    fn resolves_tilde_alone_to_full_field_range() {
        // Arrange / Act:
        let s = spec("~ * * * *");
        let minute = find_set_bit(s.minute);
        // Assert:
        assert!(minute >= 0);
        assert!(minute <= 59);
    }

    #[test]
    fn resolves_tilde_b_as_min_tilde_b() {
        // Arrange / Act:
        let s = spec("~15 * * * *");
        let minute = find_set_bit(s.minute);
        // Assert:
        assert!(minute >= 0);
        assert!(minute <= 15);
    }

    #[test]
    fn resolves_a_tilde_as_a_tilde_max() {
        // Arrange / Act:
        let s = spec("15~ * * * *");
        let minute = find_set_bit(s.minute);
        // Assert:
        assert!(minute >= 15);
        assert!(minute <= 59);
    }

    #[test]
    fn resolves_multiple_random_fields_independently() {
        // Arrange / Act:
        let s = spec("0~30 0~12 * * *");
        // Assert:
        assert!(find_set_bit(s.minute) <= 30);
        assert!(find_set_bit(s.hour) <= 12);
    }

    #[test]
    fn resolves_tilde_in_dom_field_with_correct_bounds() {
        // Arrange / Act:
        let s = spec("0 0 ~ * *");
        let dom = find_set_bit(s.dom);
        // Assert:
        assert!(dom >= 1);
        assert!(dom <= 31);
    }

    #[test]
    fn resolves_tilde_in_month_field_with_correct_bounds() {
        // Arrange / Act:
        let s = spec("0 0 1 ~ *");
        let month = find_set_bit(s.month);
        // Assert:
        assert!(month >= 1);
        assert!(month <= 12);
    }

    #[test]
    fn resolves_tilde_in_dow_field_with_correct_bounds() {
        // Arrange / Act:
        let s = spec("0 0 * * ~");
        let dow = find_set_bit(s.dow);
        // Assert:
        assert!(dow >= 0);
        assert!(dow <= 6);
    }

    #[test]
    fn preserves_tz_prefix_through_resolution() {
        // Arrange / Act:
        let s = spec("TZ=America/New_York 0~30 * * * *");
        // Assert:
        assert_eq!(
            s.location,
            Some("America/New_York".parse::<chrono_tz::Tz>().unwrap())
        );
    }

    #[test]
    fn preserves_non_random_fields() {
        // Arrange / Act:
        let s = spec("0~30 10 * * *");
        // Assert:
        assert_ne!(s.hour & (1 << 10), 0);
    }

    #[test]
    fn resolves_to_a_stable_value_across_repeated_next_calls() {
        // Arrange:
        let sched = parse_crontab("0~30 * * * *").unwrap();
        let reference = at(2025, 1, 1, 12, 0, 0);
        // Act:
        let first = sched.next(reference).unwrap();
        // Assert: the resolved random value does not change between calls.
        for _ in 0..50 {
            assert_eq!(sched.next(reference).unwrap(), first);
        }
    }

    #[test]
    fn rejects_min_beyond_max() {
        // Arrange / Act:
        let e = parse_crontab("30~0 * * * *").unwrap_err();
        // Assert:
        assert!(e.message.contains("beyond end"));
    }

    #[test]
    fn rejects_value_above_field_maximum() {
        // Arrange / Act:
        let e = parse_crontab("0~60 * * * *").unwrap_err();
        // Assert:
        assert!(e.message.contains("above maximum"));
    }

    #[test]
    fn rejects_value_below_field_minimum() {
        // Arrange / Act:
        let e = parse_crontab("0 0 0~15 * *").unwrap_err();
        // Assert:
        assert!(e.message.contains("below minimum"));
    }

    #[test]
    fn rejects_tilde_mixed_with_comma_list() {
        // Arrange / Act:
        let e = parse_crontab("0~30,45 * * * *").unwrap_err();
        // Assert:
        assert!(e.message.contains("cannot be combined"));
    }

    #[test]
    fn rejects_tilde_mixed_with_slash_step() {
        // Arrange / Act:
        let e = parse_crontab("0~30/5 * * * *").unwrap_err();
        // Assert:
        assert!(e.message.contains("cannot be combined"));
    }

    #[test]
    fn rejects_descriptor_with_tilde() {
        // Arrange / Act:
        let e = parse_crontab("@every 0~30m").unwrap_err();
        // Assert:
        assert!(e.message.contains("descriptor"));
    }

    #[test]
    fn rejects_wrong_number_of_fields() {
        // Arrange / Act:
        assert!(parse_crontab("0~30 * *").is_err());
        // Assert: (covered by the unwrap-free assertion above)
    }

    #[test]
    fn rejects_non_numeric_range_values() {
        // Arrange / Act:
        let result = parse_crontab("a~b * * * *");
        // Assert:
        assert!(result.is_err());
    }
}
