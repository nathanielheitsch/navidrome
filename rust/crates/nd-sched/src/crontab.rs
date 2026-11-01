//! Port of `scheduler/crontab_schedule.go` — `ParseCrontab`, the crontab(5)
//! random `~` syntax, and the schedule cursor used by robfig/cron.
//!
//! Reuses `nd-conf`'s `crontab.validate_crontab` (issue #03) for the *acceptance*
//! half of the contract, so `Scanner.Schedule` / `Backup.Schedule` validation
//! (`conf/configuration.go:884-910`) and this parser can never disagree about
//! which strings are valid — see `test.rs::agrees_with_nd_conf_validator`.
//! This module adds what the validator deliberately leaves out: the actual
//! parse into a schedule, and resolving the random `~` syntax once at parse
//! time (`scheduler/crontab_schedule.go:17,49-73,88-122`).

use crate::crontab_bounds;

use chrono_tz;

use nd_conf;

use rand;

use regex;

/// A parsed schedule — Rust equivalent of robfig's `cron.SpecSchedule`
/// (second/minute/hour/dom/month/dow bitsets + year) and of
/// `cron.ConstantDelaySchedule` (via [`CronSchedule.delay`]).
///
/// A field is `None` when it is `*` (robfig's starBit), otherwise the sorted,
/// deduplicated set of concrete values it allows.
pub struct CronSchedule {
    /// `Some(d)` for the constant-delay form (`5m`, `@every 30s`, `@every 50ms`).
    pub delay: DurationOption;
    /// Seconds allowed, `None` = `*` (5-field specs pin this to `[0]`, robfig's
    /// `SecondOptional`).
    pub sec: OptionVec;
    pub min: OptionVec;
    pub hour: OptionVec;
    pub dom: OptionVec;
    pub month: OptionVec;
    /// Day of week, cron numbering: 0 = Sunday … 6 = Saturday.
    pub dow: OptionVec;
    /// robfig's `domStar`/`dowStar` flags: whether each day field was `*`.
    pub dom_star: Boolean;
    pub dow_star: Boolean;
    /// Timezone taken from the `TZ=` / `CRON_TZ=` prefix, if any.
    pub tz: TimezoneOption;
}

/// Parses a cron expression, port of `ParseCrontab`
/// (`scheduler/crontab_schedule.go:20-73`).
pub fn parse_crontab(spec: String) -> (CronSchedule, Error);
