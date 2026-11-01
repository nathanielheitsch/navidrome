//! Crontab validation for `Scanner.Schedule` and `Backup.Schedule`
//! (conf/configuration.go:884-910), compatible with the Go
//! `scheduler.ParseCrontab` behavior used there:
//!
//! * a duration string (`"5m"`) is accepted as `@every 5m`;
//! * `@`-descriptors (`@hourly`, `@daily`, `@weekly`, `@monthly`, `@yearly`,
//!   `@every <duration>`, `@midnight`, `@annually`) are accepted;
//! * standard 5-field crontab expressions are accepted (seconds field
//!   optional via the Go `SecondOptional` flag);
//! * the crontab(5) random `~` syntax is *not* accepted by this validator
//!   (`scheduler.ParseCrontab` resolves `~` to a random value; resolving
//!   randomness belongs to the `nd-sched` port, which owns the parser).

use crate::duration::parse_go_duration;

/// Error returned by [`validate_crontab`].
#[derive(Debug, thiserror::Error)]
#[error("invalid schedule {input:?} (see https://pkg.go.dev/github.com/robfig/cron#hdr-CRON_Expression_Format): {message}")]
pub struct CrontabError {
    /// The offending schedule string.
    pub input: String,
    /// Human-readable reason.
    pub message: String,
}

/// Validates a schedule expression the way `scheduler.ParseCrontab`
/// (scheduler/crontab_schedule.go:20-31) does for the config keys that reach
/// it: duration strings, `@` descriptors, and standard 5/6-field crontab
/// expressions. Returns `Ok(())` when valid.
pub fn validate_crontab(spec: &str) -> Result<(), CrontabError> {
    let err = |msg: String| CrontabError {
        input: spec.to_string(),
        message: msg,
    };
    if spec.is_empty() {
        return Err(err("empty spec string".into()));
    }
    // Duration strings become @every specs.
    if parse_go_duration(spec).is_ok() {
        return Ok(());
    }
    if let Some(rest) = spec.strip_prefix('@') {
        // Descriptors accepted by the robfig parser with Descriptor flag.
        let (name, arg) = match rest.split_once(char::is_whitespace) {
            Some((n, a)) => (n, Some(a.trim())),
            None => (rest, None),
        };
        match name {
            "yearly" | "annually" | "monthly" | "weekly" | "daily" | "midnight" | "hourly" => {
                if arg.is_some() {
                    return Err(err(format!("descriptor @{name} takes no argument")));
                }
                return Ok(());
            }
            "every" => {
                let Some(arg) = arg else {
                    return Err(err("expected duration or 'none' after @every".into()));
                };
                if arg == "none" {
                    return Ok(());
                }
                if parse_go_duration(arg).is_err() {
                    return Err(err(format!("invalid duration in @every: {arg:?}")));
                }
                return Ok(());
            }
            _ => return Err(err(format!("unknown descriptor @{name}"))),
        }
    }
    // TZ=/CRON_TZ= prefix handling (scheduler/crontab_schedule.go:33-42).
    let mut body = spec;
    if let Some(stripped) = spec
        .strip_prefix("TZ=")
        .or_else(|| spec.strip_prefix("CRON_TZ="))
    {
        match stripped.find(' ') {
            Some(i) => body = stripped[i..].trim_start(),
            None => return Err(err("missing spec after timezone".into())),
        }
    }
    let fields: Vec<&str> = body.split_whitespace().collect();
    match fields.len() {
        5 | 6 => {}
        n => return Err(err(format!("expected 5 or 6 fields, found {n}"))),
    }
    for (i, field) in fields.iter().enumerate() {
        // 6-field form's first field is seconds; a 5-field form is robfig's
        // `SecondOptional`, so its fields line up with bounds indices 1..=5.
        let (min, max) = FIELD_BOUNDS[i + (6 - fields.len())];
        // (seconds bounds apply to the first field of the 6-field form)
        let kind = if fields.len() == 6 && i == 0 {
            "seconds"
        } else {
            "field"
        };
        if !valid_field(field, min, max) {
            return Err(err(format!("invalid {kind} {field:?}")));
        }
    }
    Ok(())
}

/// Inclusive bounds of each cron field, matching the Go `fieldBounds` table
/// (`scheduler/crontab_schedule.go:47-52`). robfig rejects out-of-range atoms,
/// so `"99 * * * * *"` is invalid.
const FIELD_BOUNDS: [(u32, u32); 6] = [(0, 59), (0, 59), (0, 23), (1, 31), (1, 12), (0, 6)];

/// One crontab field: `*`, ranges `a-b`, steps `x/y`, lists `a,b,c`, names
/// (months/days), all case-insensitive — a subset check compatible with the
/// robfig standard parser. Numeric atoms must fall inside `min..max`.
fn valid_field(field: &str, min: u32, max: u32) -> bool {
    if field.is_empty() {
        return false;
    }
    for part in field.split(',') {
        let part = part.trim();
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => (r, Some(s)),
            None => (part, None),
        };
        if let Some(s) = step {
            if s.is_empty() || s.chars().any(|c| !c.is_ascii_digit()) {
                return false;
            }
        }
        if range == "*" {
            continue;
        }
        let (lo, hi) = match range.split_once('-') {
            Some((lo, hi)) => (Some(lo), Some(hi)),
            None => (Some(range), None),
        };
        for atom in [lo, hi].into_iter().flatten() {
            if atom.chars().all(|c| c.is_ascii_digit()) {
                let v = atom.parse::<u32>().unwrap_or(u32::MAX);
                if v < min || v > max {
                    return false;
                }
            } else if !valid_atom(atom) {
                return false;
            }
        }
    }
    true
}

/// A single atom: number or 3-letter name (`sun`, `jan`, …).
fn valid_atom(a: &str) -> bool {
    if a.is_empty() {
        return false;
    }
    if a.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    let lower = a.to_ascii_lowercase();
    lower.len() == 3 && lower.chars().all(|c| c.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_schedules() {
        for s in [
            "0",
            "5m",
            "@every 1h",
            "@daily",
            "@hourly",
            "*/30 * * * *",
            "30 4 * * *",
            "* * * * * *",
            "0 0 1 1 *",
            "TZ=UTC 0 0 * * *",
        ] {
            assert!(validate_crontab(s).is_ok(), "expected valid: {s}");
        }
    }

    #[test]
    fn rejects_invalid_schedules() {
        for s in [
            "",
            "junk",
            "* * * *",
            "@bogus",
            "*/x * * * *",
            "99 * * * * *",
        ] {
            assert!(validate_crontab(s).is_err(), "expected invalid: {s}");
        }
    }
}
