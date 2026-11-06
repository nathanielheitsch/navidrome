//! Port of the robfig/cron schedule cursor (`spec.go`, `constantdelay.go`)
//! consumed by `scheduler/crontab_schedule.go`, plus `scheduler/scheduler.go`
//! (the entry runner: add/remove/run/stop + the process-wide singleton).
//!
//! Go's `SpecSchedule.Next` walks month → day → hour → minute → second,
//! re-verifying from the top whenever a field wraps (the `WRAP` goto). The
//! port below performs the same walk on wall-clock components in the
//! schedule's `TZ=`/`CRON_TZ=` location (default: the caller's clock), then
//! resolves the wall time back to an instant, shifting forward across DST
//! gaps the way Go's day-loop hour adjustment does.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration as StdDuration;

use chrono::{
    DateTime, Datelike, Duration, Months, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc,
};
use chrono_tz::Tz;

use crate::crontab::CrontabError;
use crate::log_adapter::CronLogger;

/// A parsed duty cycle: the port of the robfig `cron.Schedule` interface as
/// an enum of its two concrete implementations.
#[derive(Debug, Clone)]
pub enum Schedule {
    /// Traditional crontab specification (bit sets, second granularity).
    Spec(SpecSchedule),
    /// `@every <duration>`: fires on every whole-second grid point.
    Delay(DelaySchedule),
}

impl Schedule {
    /// Port of `Schedule.Next`: the next activation time strictly after
    /// `after` (Go returns the zero time when unsatisfiable within the
    /// 5-year search limit; the port returns `None`).
    pub fn next(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self {
            Schedule::Spec(spec) => spec.next(after),
            Schedule::Delay(delay) => Some(delay.next(after)),
        }
    }
}

/// Set when a field expression contained `*` or `?` (robfig `starBit`).
pub(crate) const STAR_BIT: u64 = 1 << 63;

/// Port of robfig `SpecSchedule` (spec.go): second-granularity duty cycle
/// stored as bit sets. Bit `n` of `minute` is set when minute `n` matches,
/// and so on; `dom` uses day-of-month (1–31), `dow` Sunday=0…Saturday=6.
#[derive(Debug, Clone)]
pub struct SpecSchedule {
    /// Matching seconds (bit index = second, 0–59).
    pub second: u64,
    /// Matching minutes (bit index = minute, 0–59).
    pub minute: u64,
    /// Matching hours (bit index = hour, 0–23).
    pub hour: u64,
    /// Matching days of month (bit index = day, 1–31).
    pub dom: u64,
    /// Matching months (bit index = month, 1–12).
    pub month: u64,
    /// Matching days of week (bit index = weekday, 0=Sunday).
    pub dow: u64,
    /// `TZ=`/`CRON_TZ=` override; `None` is robfig's `time.Local` (use the
    /// clock the caller handed in).
    pub location: Option<Tz>,
}

impl SpecSchedule {
    /// Port of `SpecSchedule.Next` (spec.go:44-126): the next-match walk.
    /// Month, day, hour, minute and second are each advanced to their next
    /// matching value; any wrap restarts from the month check. The search
    /// gives up (returns `None`) past Go's five-year limit.
    pub fn next(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        // Express the starting instant as a wall clock in the schedule's
        // location (the caller's clock when no TZ was given, since `after`
        // is UTC here). The walk itself is pure wall-clock arithmetic.
        let after_local: NaiveDateTime = match self.location {
            Some(tz) => tz.from_utc_datetime(&after.naive_utc()).naive_local(),
            None => after.naive_utc(),
        };

        // Start at the earliest possible time: the upcoming second.
        let mut t = after_local + Duration::seconds(1)
            - Duration::nanoseconds(after_local.nanosecond() as i64);
        // Whether a field has been incremented (fields below reset to 0).
        let mut added = false;
        // If no time is found within five years, return the zero time.
        let year_limit = t.year() + 5;

        'wrap: loop {
            if t.year() > year_limit {
                return None;
            }

            // Find the first applicable month; reset day/time when moving.
            while (1u64 << t.month()) & self.month == 0 {
                if !added {
                    added = true;
                    t = NaiveDate::from_ymd_opt(t.year(), t.month(), 1)
                        .unwrap()
                        .and_hms_opt(0, 0, 0)
                        .unwrap();
                }
                t = add_months(t, 1);
                // Wrapped around into a new year.
                if t.month() == 1 {
                    continue 'wrap;
                }
            }

            // Now get a day in that month.
            while !self.day_matches(t) {
                if !added {
                    added = true;
                    t = t.date().and_hms_opt(0, 0, 0).unwrap();
                }
                t += Duration::days(1);
                if t.day() == 1 {
                    continue 'wrap;
                }
            }

            while (1u64 << t.hour()) & self.hour == 0 {
                if !added {
                    added = true;
                    t = t.date().and_hms_opt(t.hour(), 0, 0).unwrap();
                }
                t += Duration::hours(1);
                if t.hour() == 0 {
                    continue 'wrap;
                }
            }

            while (1u64 << t.minute()) & self.minute == 0 {
                if !added {
                    added = true;
                    t = t - Duration::seconds(t.second() as i64);
                }
                t += Duration::minutes(1);
                if t.minute() == 0 {
                    continue 'wrap;
                }
            }

            while (1u64 << t.second()) & self.second == 0 {
                if !added {
                    added = true;
                    t = t - Duration::nanoseconds(t.nanosecond() as i64);
                }
                t += Duration::seconds(1);
                if t.second() == 0 {
                    continue 'wrap;
                }
            }

            break;
        }

        Some(resolve_instant(t, self.location))
    }

    /// Port of `dayMatches` (spec.go:128-136): with a `*` on either day
    /// field both must match; restricted (non-star) day fields are OR-ed,
    /// the crontab(5) rule.
    fn day_matches(&self, t: NaiveDateTime) -> bool {
        let dom_match = (1u64 << t.day()) & self.dom > 0;
        let dow_match = (1u64 << t.weekday().num_days_from_sunday()) & self.dow > 0;
        if self.dom & STAR_BIT > 0 || self.dow & STAR_BIT > 0 {
            dom_match && dow_match
        } else {
            dom_match || dow_match
        }
    }
}

/// Port of Go's `AddDate(0, n, 0)` month normalization: the first of the
/// next month plus the original day offset (so Jan 31 + 1 month lands in
/// March, not clamped to Feb 28).
fn add_months(t: NaiveDateTime, n: u32) -> NaiveDateTime {
    let first_of_month = NaiveDate::from_ymd_opt(t.year(), t.month(), 1)
        .unwrap()
        .checked_add_months(Months::new(n))
        .unwrap();
    (first_of_month + Duration::days(t.day() as i64 - 1)).and_time(t.time())
}

/// Resolve a matched wall-clock time back to a UTC instant. Ambiguous
/// times (DST fold) take the earlier reading; times inside a DST gap shift
/// forward hour by hour, mirroring Go's day-loop hour adjustment.
fn resolve_instant(naive: NaiveDateTime, location: Option<Tz>) -> DateTime<Utc> {
    let Some(tz) = location else {
        return Utc.from_utc_datetime(&naive);
    };
    let mut candidate = naive;
    loop {
        match tz.from_local_datetime(&candidate) {
            chrono::LocalResult::Single(dt) => return dt.with_timezone(&Utc),
            chrono::LocalResult::Ambiguous(dt, _) => return dt.with_timezone(&Utc),
            chrono::LocalResult::None => candidate += Duration::hours(1),
        }
    }
}

/// Port of robfig `ConstantDelaySchedule` (constantdelay.go): fires once
/// per whole-second delay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelaySchedule {
    /// Whole-second delay between activations.
    pub delay: Duration,
}

impl DelaySchedule {
    /// Port of `ConstantDelaySchedule.Next`: `after + delay`, truncated to
    /// the whole-second grid.
    pub fn next(&self, after: DateTime<Utc>) -> DateTime<Utc> {
        after + self.delay - Duration::nanoseconds(after.timestamp_subsec_nanos() as i64)
    }
}

/// Port of robfig `Every` (constantdelay.go): delays under a second round
/// up to one second; sub-second remainder is truncated.
pub fn every(duration: StdDuration) -> DelaySchedule {
    let d = if duration < StdDuration::from_secs(1) {
        Duration::from_std(StdDuration::from_secs(1)).unwrap()
    } else {
        Duration::from_std(duration).unwrap()
    };
    DelaySchedule {
        delay: d - Duration::nanoseconds(d.num_nanoseconds().unwrap() % 1_000_000_000),
    }
}

/// Identifies an entry within a [`Scheduler`] (Go `cron.EntryID`). The first
/// allocated id is nonzero, matching robfig.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryId(u32);

impl EntryId {
    /// The zero id, matching robfig's `Entry.Valid() == false`.
    pub fn is_zero(&self) -> bool {
        self.0 == 0
    }
}

/// A scheduled job: shared so the runner can execute it without holding
/// the state lock.
type Job = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone)]
struct Entry {
    schedule: Schedule,
    job: Job,
    next: Option<DateTime<Utc>>,
}

struct Inner {
    entries: BTreeMap<u32, Entry>,
    next_id: u32,
    stop: bool,
}

struct Shared {
    state: Mutex<Inner>,
    cv: Condvar,
}

/// Port of `scheduler.Scheduler` (scheduler.go): a runner that fires
/// registered jobs on their [`Schedule`]. Clones share the same entries
/// (the Go value is a singleton pointer).
#[derive(Clone)]
pub struct Scheduler {
    shared: Arc<Shared>,
    logger: Arc<dyn CronLogger>,
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl Scheduler {
    /// A fresh scheduler with no entries (Go's `cron.New` + the scheduler
    /// constructor, minus the singleton).
    pub fn new() -> Scheduler {
        Scheduler::with_logger(Arc::new(crate::log_adapter::Logger))
    }

    /// Build with a specific [`CronLogger`] (Go injects `cron.WithLogger`).
    pub fn with_logger(logger: Arc<dyn CronLogger>) -> Scheduler {
        Scheduler {
            shared: Arc::new(Shared {
                state: Mutex::new(Inner {
                    entries: BTreeMap::new(),
                    next_id: 1,
                    stop: false,
                }),
                cv: Condvar::new(),
            }),
            logger,
        }
    }

    /// Port of `scheduler.GetInstance`: the process-wide scheduler.
    pub fn instance() -> Scheduler {
        static INSTANCE: OnceLock<Scheduler> = OnceLock::new();
        INSTANCE.get_or_init(Scheduler::new).clone()
    }

    /// Port of `Scheduler.Add`: parse the crontab spec and register the
    /// job. The entry fires on its schedule once [`Scheduler::run`] is
    /// active; the returned id is nonzero and can be passed to
    /// [`Scheduler::remove`].
    pub fn add<F>(&self, crontab: &str, cmd: F) -> Result<EntryId, CrontabError>
    where
        F: Fn() + Send + Sync + 'static,
    {
        let schedule = crate::crontab::parse_crontab(crontab)?;
        let mut state = self.shared.state.lock().expect("scheduler state poisoned");
        let id = state.next_id;
        state.next_id += 1;
        state.entries.insert(
            id,
            Entry {
                schedule,
                job: Arc::new(cmd),
                next: None,
            },
        );
        drop(state);
        self.shared.cv.notify_all();
        Ok(EntryId(id))
    }

    /// Port of `Scheduler.Remove`: drop the entry with the given id.
    pub fn remove(&self, id: EntryId) {
        let mut state = self.shared.state.lock().expect("scheduler state poisoned");
        state.entries.remove(&id.0);
        drop(state);
        self.shared.cv.notify_all();
    }

    /// Port of `Scheduler.Run` (blocking on the caller, like `ctx.Done()`):
    /// run the dispatch loop until [`Scheduler::stop`] is called. Jobs run
    /// on their own threads, matching robfig's goroutine-per-job.
    pub fn run(&self) {
        let mut state = self.shared.state.lock().expect("scheduler state poisoned");
        self.logger.info("start", &[]);
        loop {
            if state.stop {
                break;
            }
            let now = Utc::now();
            let mut due: Vec<Job> = Vec::new();
            let mut soonest: Option<DateTime<Utc>> = None;
            for entry in state.entries.values_mut() {
                if entry.next.is_none() {
                    entry.next = entry.schedule.next(now);
                }
                match entry.next {
                    Some(n) if n <= now => {
                        due.push(Arc::clone(&entry.job));
                        entry.next = entry.schedule.next(now);
                    }
                    Some(n) => {
                        soonest = Some(match soonest {
                            Some(s) => s.min(n),
                            None => n,
                        })
                    }
                    None => {}
                }
            }
            if due.is_empty() {
                let wait = match soonest {
                    Some(n) => (n - Utc::now())
                        .to_std()
                        .unwrap_or(StdDuration::from_millis(1))
                        .min(StdDuration::from_secs(3600)),
                    None => StdDuration::from_secs(3600),
                };
                let (guard, _) = self
                    .shared
                    .cv
                    .wait_timeout(state, wait.max(StdDuration::from_millis(1)))
                    .expect("scheduler state poisoned");
                state = guard;
            } else {
                drop(state);
                for job in due {
                    std::thread::spawn(move || job());
                }
                state = self.shared.state.lock().expect("scheduler state poisoned");
            }
        }
        self.logger.info("stop", &[]);
    }

    /// Unblock [`Scheduler::run`] (Go's `ctx.Done()` counterpart). Jobs
    /// already running are not interrupted, matching `cron.Stop`.
    pub fn stop(&self) {
        let mut state = self.shared.state.lock().expect("scheduler state poisoned");
        state.stop = true;
        drop(state);
        self.shared.cv.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, mi, s).unwrap()
    }

    #[test]
    fn next_match_list_form() {
        // Arrange: a list of minutes, from a time past all of today's
        // matches before the listed hours.
        // Act:
        let schedule = crate::crontab::parse_crontab("0,30 3 * * *").unwrap();
        let next = schedule.next(at(2025, 6, 1, 12, 0, 0)).unwrap();
        // Assert: the next listed minute of the next listed hour.
        assert_eq!(next, at(2025, 6, 2, 3, 0, 0));
    }

    #[test]
    fn next_match_step_form() {
        // Arrange: a step expression, from a time between steps.
        // Act:
        let schedule = crate::crontab::parse_crontab("*/15 * * * *").unwrap();
        let next = schedule.next(at(2025, 1, 1, 12, 7, 30)).unwrap();
        // Assert: the next quarter-hour boundary.
        assert_eq!(next, at(2025, 1, 1, 12, 15, 0));
    }

    #[test]
    fn next_match_range_form() {
        // Arrange: a minute range, on an hour outside the schedule.
        // Act:
        let schedule = crate::crontab::parse_crontab("10-20 0 * * *").unwrap();
        let next = schedule.next(at(2025, 6, 1, 5, 7, 0)).unwrap();
        // Assert: the first minute inside the range, next matching hour.
        assert_eq!(next, at(2025, 6, 2, 0, 10, 0));
    }

    #[test]
    fn next_match_six_field_seconds() {
        // Arrange: a 6-field spec pinning seconds, from a sub-range time.
        // Act:
        let schedule = crate::crontab::parse_crontab("30 0 12 * * *").unwrap();
        let next = schedule.next(at(2025, 6, 1, 12, 0, 31)).unwrap();
        // Assert: the next day's only matching second.
        assert_eq!(next, at(2025, 6, 2, 12, 0, 30));
    }

    #[test]
    fn next_is_strictly_after() {
        // Arrange: an exactly-matching instant; a 5-field spec prepends a
        // `0` second (robfig `SecondOptional`), so the every-second form
        // is the explicit 6-field spec (pinned against robfig v3.0.1).
        // Act:
        let schedule = crate::crontab::parse_crontab("* * * * * *").unwrap();
        let next = schedule.next(at(2025, 1, 1, 12, 0, 0)).unwrap();
        // Assert: the following second, not the match itself.
        assert_eq!(next, at(2025, 1, 1, 12, 0, 1));
    }

    #[test]
    fn next_walks_across_days_and_months() {
        // Arrange: a spec matching only one specific day-of-month.
        // Act:
        let schedule = crate::crontab::parse_crontab("0 0 1 * *").unwrap();
        let next = schedule.next(at(2025, 1, 15, 10, 0, 0)).unwrap();
        // Assert: the first of the next month.
        assert_eq!(next, at(2025, 2, 1, 0, 0, 0));
    }

    #[test]
    fn next_resolves_in_schedule_timezone() {
        // Arrange: a spec with a TZ prefix; New York is UTC-5 in January.
        // Act:
        let schedule = crate::crontab::parse_crontab("TZ=America/New_York 0 12 * * *").unwrap();
        let next = schedule.next(at(2025, 1, 1, 0, 0, 0)).unwrap();
        // Assert: noon EST is 17:00 UTC.
        assert_eq!(next, at(2025, 1, 1, 17, 0, 0));
    }

    #[test]
    fn delay_schedule_rounds_to_the_second_grid() {
        // Arrange: a constant delay, from a mid-second time.
        // Act:
        let base = at(2025, 1, 1, 12, 0, 30) + Duration::nanoseconds(500_000_000);
        let schedule = crate::crontab::parse_crontab("@every 1m30s").unwrap();
        let next = schedule.next(base).unwrap();
        // Assert: whole seconds only, delay applied.
        assert_eq!(next, at(2025, 1, 1, 12, 2, 0));
    }

    // Ports of scheduler/scheduler_test.go below.

    #[test]
    fn scheduler_adds_and_executes_a_job() {
        // Arrange: a fresh scheduler running on its own thread.
        let scheduler = Scheduler::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let runner = scheduler.clone();
        let handle = std::thread::spawn(move || runner.run());

        // Act:
        let result = scheduler.add("@every 50ms", move || {
            let _ = tx.send(());
        });

        // Assert: the job was registered with a nonzero id and ran.
        assert!(result.is_ok());
        assert!(!result.unwrap().is_zero());
        assert!(
            rx.recv_timeout(StdDuration::from_secs(5)).is_ok(),
            "job should have executed"
        );

        scheduler.stop();
        handle.join().unwrap();
    }

    #[test]
    fn scheduler_adds_a_job_with_random_tilde_syntax() {
        // Arrange: a running scheduler (Go's BeforeEach starts it).
        let scheduler = Scheduler::new();
        let runner = scheduler.clone();
        let handle = std::thread::spawn(move || runner.run());

        // Act:
        let result = scheduler.add("0~59 * * * *", || {});

        // Assert: registration succeeded with a nonzero id, and removal works.
        assert!(result.is_ok());
        let id = result.unwrap();
        assert!(!id.is_zero());
        scheduler.remove(id);
        assert!(scheduler.add("~ * * * *", || {}).is_ok());

        scheduler.stop();
        handle.join().unwrap();
    }

    #[test]
    fn scheduler_instance_is_a_singleton() {
        // Arrange: two fetches of the process-wide scheduler.
        // Act:
        let a = Scheduler::instance();
        let b = Scheduler::instance();
        // Assert: same underlying entries (Arc pointer equality).
        assert!(Arc::ptr_eq(&a.shared, &b.shared));
    }
}
