//! #04 — DB open/init plus the single fresh-schema migration.
//!
//! Go side: `db/db.go`. The custom sqlite driver's `ConnectHook` (`db/db.go:44-51`)
//! registers `SEEDEDRAND` (`utils/hasher.HashFunc()`) and the `NATSORT` collation
//! (`utils/natural.CompareFold`) **per connection** — contract §4 of
//! `docs/rust-port/schema-contract.md` requires re-registering both on every Rust
//! connection. `:memory:` is rewritten (`db/db.go:54-55`); the DDL pass runs with
//! foreign keys off then back on (`db/db.go:85-91`). Per contract §0 there is ONE
//! fresh migration ([`crate::schema_ddl`]) — no goose history, no upgrade path.
//!
//! DB-03 (D-OPEN decision, option a): Go's DSN pragmas are **go-sqlite3
//! conventions**, not SQLite URI parameters, so rusqlite applies none of them.
//! [`parse_open_input`] reads a Go-style DSN with go-sqlite3's own semantics and
//! [`DbOpenOptions`] carries what it resolves to; [`apply_open_options`] sets each
//! setting with an explicit per-connection `PRAGMA` and reads the effective state
//! back, so every pooled handle starts in the state Go's handle is in.
//!
//! Two different things are configured, and conflating them was the DB-03 review
//! finding (go-sqlite3 v1.14.52, verified by running the driver: `sql.Open` +
//! `SELECT 1` + `PRAGMA` read-back):
//!   * the **driver's own state** for any path nobody configures — journal mode
//!     unset (`journalMode` empty, so no `PRAGMA journal_mode`, `sqlite3.go:1236`),
//!     `busy_timeout = 5000` (`:1231`), foreign keys unset (`foreignKeys = -1`, so
//!     no `PRAGMA foreign_keys`, `:1234`), `synchronous = NORMAL` always exec'd
//!     (`:1241`, `:1906`). Measured: a bare path gives `delete` / 5000 / sync 1 /
//!     fk 0. [`DbOpenOptions::driver_defaults`] models exactly that.
//!   * **Navidrome's preset**, which comes from `conf.Server.DbPath`, not from the
//!     driver: `conf/configuration.go:404-406` only substitutes
//!     `consts.DefaultDbPath` (`consts/consts.go:14`) when `DbPath` is empty, and
//!     `db/db.go:54-55` rewrites `:memory:`. That preset is WAL / 15000 / NORMAL /
//!     FK-on ([`DbOpenOptions::navidrome_default_options`]) and applies to the
//!     default DB path only — a custom `DbPath` gets the driver state plus whatever
//!     its own query asks for.
//!
//! On this Rust build (rusqlite 0.32.1 against the SQLite amalgamation
//! `libsqlite3-sys` 0.30.1 compiles, SQLite 3.46.0) an unset handle reports
//! `journal_mode=delete` and `synchronous=2`; `busy_timeout = 5000` is installed by
//! rusqlite itself (`rusqlite/src/inner_connection.rs:119`), matching the driver's
//! 5000. `foreign_keys` reads **1** even on a plain `rusqlite::Connection::open`,
//! because that build passes `-DSQLITE_DEFAULT_FOREIGN_KEYS=1`
//! (`libsqlite3-sys-0.30.1/build.rs:123`) — a compile-time default, NOT
//! go-sqlite3's behavior (Go leaves FK alone, measured fk=0). The port therefore
//! leaves FK unset unless the DSN asks for it, so SQLite's compiled default stays
//! in charge exactly as it does today.

use crate::schema_ddl::{FRESH_STATEMENTS, TABLE_NAMES};
use rusqlite::functions::{Context, FunctionFlags};
use rusqlite::{Connection, Error as RsError, OpenFlags};

use std::cmp::Ordering;

use nd_utils::hasher::Hasher;
use nd_utils::natural::compare_fold;

/// Contract §4: collation name Go registers (`db/db.go:24`).
pub const NATSORT_COLLATION: &str = "NATSORT";

/// Go-side DSN rewrite for `:memory:` (`db/db.go:54-55`).
///
/// The rewrite is the exact test `Path == ":memory:"` — a bare path with no query
/// and no `file:` prefix (measured with go-sqlite3 v1.14.52, two handles each):
///   * Without Navidrome's rewrite, SQLite's bare `:memory:` and `file::memory:`
///     are private (second handle: `no such table`). Only the exact bare input
///     is rewritten to the shared-cache URI by Navidrome.
///   * `:memory:?_fk=off` does not match the test, so Go leaves it private too.
pub const MEMORY_PATH: &str = ":memory:";
/// The shared-cache memory URI Go rewrites `:memory:` to (`db/db.go:54-55`, which
/// also writes it back into `conf.Server.DbPath` at `:55`). [`parse_open_input`]
/// extracts `_foreign_keys=on` into [`DbOpenOptions`] while handing this `file:`
/// URI to SQLite unchanged. SQLite itself ignores the underscore convention.
pub const MEMORY_DSN: &str = "file::memory:?cache=shared&_foreign_keys=on";

/// The domain of `PRAGMA busy_timeout`, and why a value outside it reads back as 0.
///
/// go-sqlite3 parses the DSN value as `int64` (`strconv.ParseInt`,
/// `sqlite3.go:1346-1350`, stored into `busyTimeout` at `:1231` and `:1350`) and hands
/// it to SQLite as TEXT: `PRAGMA busy_timeout = %d;` (`sqlite3.go:1665`). SQLite runs
/// that text through `sqlite3Atoi` (`sqlite3.c:141340`), i.e. `sqlite3GetInt32`, which
/// gives up — returning 0 with no error — when the digits exceed 10 or the magnitude
/// exceeds `INT_MAX` (`sqlite3.c:35858-35910`, `sqlite3Atoi` `:35917-35921`), and a
/// non-positive timeout installs no busy handler at all, so the pragma reports 0
/// (`sqlite3_busy_timeout` `sqlite3.c:181612-181621`).
///
/// Measured against the driver (go-sqlite3 v1.14.52, `sql.Open` + `SELECT 1` +
/// `PRAGMA busy_timeout`) and against rusqlite on this build. The two agree on every
/// row below, and none of the first group is a rejection:
///   `_busy_timeout=2147483647`            -> 2147483647
///   `_busy_timeout=2147483648`            -> 0     (magnitude above INT_MAX)
///   `_busy_timeout=4294967296`            -> 0
///   `_busy_timeout=4294967297`            -> 0     (NOT 1: there is no 32-bit
///                                          truncation, the pragma parses TEXT and bails)
///   `_busy_timeout=10000000000`           -> 0     (11 digits)
///   `_busy_timeout=-1`                    -> 0
///   `_busy_timeout=-2147483649`           -> 0
///   `_busy_timeout=0`                     -> 0
///   `_busy_timeout=000000000000000000005` -> 5     (Go normalizes to "5" first)
///   `_busy_timeout=9223372036854775807`   -> 0
///   `_busy_timeout=9223372036854775808`   -> error (int64 overflow in Go's parser)
///   `_busy_timeout=0x10`, `1_000`, `12.5`, `+0` -> error (Go's ParseInt grammar;
///                                          SQLite's own reader would take `0x10` as 16,
///                                          but Go never sends it)
/// Upper bound of the domain that can be held; anything outside resolves to 0, which
/// is what `parse_busy_timeout` applies before a pragma is emitted.
pub const SQLITE_BUSY_TIMEOUT_MAX_MS: u64 = i32::MAX as u64;
/// SQLite URI parameters this port passes through (D-OPEN decision, option a).
/// Every name here is read by SQLite itself: `vfs`/`cache`/`mode` in
/// `sqlite3ParseUri` (declared `sqlite3.c:182763`, option loop `:182903-182965`).
/// Other readers are VFS/pager-specific: Unix (`:36601`, `:43115`, `:44188`,
/// `:44582`, `:50626`, `:62067-62069`), Windows `exclusive` (`:51816`), and RBU
/// `rbu_memory` (`:223262`). A listed option need not affect every platform/VFS.
/// Other names are Go conventions, ignored bare names, or explicitly rejected.
///
/// `cache` — and only `cache` — is also forwarded for Navidrome's own default DSN,
/// which is not a `file:` URI: see [`NAVIDROME_FORWARDED_PARAMS`].
pub const SUPPORTED_URI_PARAMS: &[&str] = &[
    "vfs",
    "cache",
    "mode",
    "immutable",
    "nolock",
    "psow",
    "readonly_shm",
    "exclusive",
    "modeof",
    "8_3_names",
    "rbu_memory",
];

/// go-sqlite3 option names spelled WITHOUT the leading underscore, which
/// go-sqlite3 does not read at all. Every option block resets `pkey` to `""` and
/// then only ever assigns an underscore-prefixed name (`sqlite3.go:1338-1345`,
/// `:1401-1408`, `:1438-1445`, `:1538-1545`), so a bare `journal_mode=`/
/// `busy_timeout=`/`synchronous=`/`foreign_keys=` never reaches a `params.Get` call
/// and there is no `pkey == ""` fallback that would. Measured against the driver
/// (sql.Open + `SELECT 1` + pragma read-back):
///   ?journal_mode=wal            -> journal=delete
///   ?busy_timeout=9999           -> busy=5000
///   ?synchronous=full            -> sync=1
///   ?foreign_keys=on             -> fk=0
/// and the bare name loses to an underscore name even alongside one
/// (`?_busy_timeout=111&busy_timeout=9999` -> 111). SQLite reads these names neither
/// as URI configuration parameters (its option reader handles only
/// `vfs`/`cache`/`mode`, `sqlite3.c:182903-182946`) nor as pragmas implied by the
/// filename, so treating them as settings would invent state Go's handle does not
/// have. Measured on BOTH sides against a `file:` URI:
///   Go and this build: `file:…?journal_mode=wal` -> journal=delete,
///   `file:…?busy_timeout=9999` -> busy=5000.
/// They are accepted and IGNORED, exactly as Go ignores them — which is also what
/// makes `consts.DefaultDbPath`'s bare `synchronous=normal` parseable at all.
pub const GO_IGNORED_BARE_NAMES: &[&str] = &[
    "journal_mode",
    "journal",
    "busy_timeout",
    "timeout",
    "synchronous",
    "sync",
    "foreign_keys",
    "fk",
];

/// go-sqlite3 DSN parameters that configure **no SQLite connection state**, so the
/// port accepts and drops them: `_txlock` only chooses the statement the driver
/// issues to start a transaction (`sqlite3.go:1296-1308`) and `_loc` only the
/// `*time.Location` its rows scan into (`:1271-1282`). Neither has a pragma, an open
/// flag, or a read-back, and no `nd-*` crate asks for either (`grep -rn
/// "_txlock\|_loc=" rust/` finds only DSN strings in Go-side Go tests). Their VALUES
/// are still validated exactly like Go's, because Go rejects a bad one before the
/// handle is usable (measured): `_txlock=invalid` -> `Invalid _txlock: invalid`,
/// `_loc=invalid` -> `Invalid _loc: invalid: unknown time zone invalid`.
pub const GO_DRIVER_ONLY_PARAMS: &[&str] = &["_txlock", "_loc"];

/// go-sqlite3 parameters that map to a `PRAGMA` the driver execs in `Open`
/// (`sqlite3.go:1810` auto_vacuum, `:1838` case_sensitive_like, `:1845`
/// defer_foreign_keys, `:1859` ignore_check_constraints, `:1874` locking_mode,
/// `:1880` query_only, `:1887` recursive_triggers, `:1898` secure_delete, `:1912`
/// writable_schema). rusqlite 0.32.1 has no per-handle equivalent of that exec
/// block, so the port validates the value with the driver's own grammar and then
/// REFUSES the parameter: applying nothing while reporting success would hand out a
/// handle whose setting was silently ignored — the exact failure mode D-OPEN exists
/// to prevent. An EMPTY value is accepted and dropped, because go-sqlite3's
/// `params.Get(pkey)` check skips the whole block then (`:1322`, `:1366`, `:1386`,
/// `:1423`, `:1471`, `:1484`, `:1506`, `:1521`, `:1558`), i.e. it provably configures
/// nothing.
pub const GO_STATE_PRAGMAS: &[&str] = &[
    "_auto_vacuum",
    "_vacuum",
    "_case_sensitive_like",
    "_cslike",
    "_defer_foreign_keys",
    "_defer_fk",
    "_ignore_check_constraints",
    "_locking_mode",
    "_locking",
    "_query_only",
    "_recursive_triggers",
    "_rt",
    "_secure_delete",
    "_writable_schema",
];

/// Unsupported pragma families use the same canonical/alias presence precedence
/// as implemented settings. Never validate a losing name's value.
const GO_STATE_ALIASES: &[(&str, &str)] = &[
    ("_auto_vacuum", "_vacuum"),
    ("_case_sensitive_like", "_cslike"),
    ("_defer_foreign_keys", "_defer_fk"),
    ("_locking_mode", "_locking"),
    ("_recursive_triggers", "_rt"),
];

/// go-sqlite3 parameters with no `PRAGMA` behind them, which still change how the
/// driver or the handle behaves: `_mutex` picks `SQLITE_OPEN_NOMUTEX`/
/// `SQLITE_OPEN_FULLMUTEX` (`sqlite3.go:1284-1294`), `_stmt_cache_size` sizes the
/// driver's statement cache (`:1569-1593`), and `_auth`/`_auth_*`
/// turn on the driver's own user-authentication layer (`:1254-1268`, applied at
/// `:1700-1733`). Validated like Go's and then refused, for the same reason as
/// [`GO_STATE_PRAGMAS`]. Note `_auth` has NO value grammar — go-sqlite3 only checks
/// PRESENCE (`if _, ok := params["_auth"]; ok`), so even `_auth=` triggers it and it
/// is refused whatever its value. Go also fails `_auth` without `_auth_user`,
/// though its error wording differs. `_cache_size` is NOT driver-only: it emits
/// `PRAGMA cache_size` (`sqlite3.go:1917-1921`) and is applied by this port.
///
/// Everything else go-sqlite3 recognizes is exactly the five settings in
/// [`GO_OPTION_KEYS`], these two lists, and `vfs` in [`SUPPORTED_URI_PARAMS`] — a
/// complete enumeration of `params.Get(...)`/`params[...]` in `Open`
/// (v1.14.52 `sqlite3.go:1247-1597`). Names outside that set (`verbose`,
/// `_to_time`, `_time_format`, `_null_time`, `_no_password`, `_fcache_size_limit`,
/// `_unlock_rowid`, `_authorize`, `_begin_behavior`, …) are NOT driver parameters at
/// all; go-sqlite3 ignores them silently, and this port rejects them as unknown.
pub const GO_DRIVER_STATE_PARAMS: &[&str] = &[
    "_mutex",
    "_stmt_cache_size",
    "_auth",
    "_auth_user",
    "_auth_pass",
    "_auth_crypt",
    "_auth_salt",
];

/// Every go-sqlite3 parameter this port **refuses**: [`GO_STATE_PRAGMAS`] plus
/// [`GO_DRIVER_STATE_PARAMS`]. Values are validated with the driver's own grammar
/// first, so a value go-sqlite3 would reject is rejected here too. An unknown name is
/// refused as well, so a typo can never open a handle with settings nobody asked
/// for. [`parse_open_input`] carries the complete policy table.
pub const GO_REJECTED_PARAMS: &[&str] = &[
    "_auto_vacuum",
    "_vacuum",
    "_case_sensitive_like",
    "_cslike",
    "_defer_foreign_keys",
    "_defer_fk",
    "_ignore_check_constraints",
    "_locking_mode",
    "_locking",
    "_query_only",
    "_recursive_triggers",
    "_rt",
    "_secure_delete",
    "_writable_schema",
    "_mutex",
    "_stmt_cache_size",
    "_auth",
    "_auth_user",
    "_auth_pass",
    "_auth_crypt",
    "_auth_salt",
];

/// The only SQLite URI parameter this port forwards for a NON-URI input, and only
/// for Navidrome's own default DSN (`nd-conf/src/consts.rs:16`), and only when the
/// path can be addressed as a `file:` URI (see [`OpenTarget::vfs`] and the
/// authority rule in [`parse_open_input`]).
///
/// go-sqlite3 strips the whole query when the DSN does not start with `file:`
/// (`sqlite3.go:1599-1601`), so **Go's SQLite never receives `cache=shared`** for
/// the default DSN — verified: the default DSN and the same DSN minus `cache=shared`
/// give byte-identical pragma state and file name. Forwarding it anyway is a
/// deliberate divergence recorded by the D-OPEN decision
/// (`docs/rust-port/subplans/db-runtime.md`, "D-OPEN as implemented") rather than
/// something inherited from an earlier package, and it is what actually reaches
/// SQLite. The tradeoff is explicit and measured on this build: a shared cache makes
/// two handles writing the same table fail immediately with
/// `SQLITE_LOCKED_SHAREDCACHE` (extended code 262, "database table is locked")
/// instead of waiting out `busy_timeout` for `SQLITE_BUSY` (extended 5) —
/// `nd_db::store::is_busy` classifies both. Nothing else is ever forwarded for a
/// non-URI input, so no go-sqlite3 convention can leak into a URI.
pub const NAVIDROME_FORWARDED_PARAMS: &[(&str, &str)] = &[("cache", "shared")];

/// SELECT exercising the SEEDEDRAND scalar function (tests only).
pub const SEEDEDRAND_SQL: &str = "SELECT SEEDEDRAND('album-1', 'song-1')";

/// Failures opening or applying the migration.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// SQLite failed to open or process the DB (path kept for diagnostics).
    #[error("cannot open db '{path}': {cause}")]
    Failed {
        /// Path or DSN of the DB that failed.
        path: String,
        /// Underlying rusqlite error.
        cause: RsError,
    },

    /// Filesystem failure around backup/restore (message carries the path).
    /// Added for #04's backup.rs: an IO failure has no rusqlite cause to
    /// smuggle into `Failed.cause`, and inventing one would misreport it.
    #[error("cannot access backup file '{message}'")]
    Filesystem {
        /// Human-readable failure including the path.
        message: String,
    },

    /// A DSN/path input this port cannot open: an unknown parameter, a malformed
    /// parameter, or a value outside the accepted set. Rejected before the handle
    /// is opened, so nothing is ever half-configured (D-OPEN decision, option a:
    /// unknown params are rejected rather than silently dropped).
    #[error("cannot use db input '{input}': {reason}")]
    InvalidDsn {
        /// The rejected path or DSN.
        input: String,
        /// Why it was rejected.
        reason: String,
    },

    /// A handle whose EFFECTIVE state differs from the requested open options.
    /// Reported with both the requested and the measured value, so opening a pool
    /// cannot pass on a pragma SQLite ignored.
    #[error("db handle '{path}' does not hold the requested settings: {reason}")]
    OpenConfiguration {
        /// Path/DSN the handle was opened against.
        path: String,
        /// Requested-vs-effective detail.
        reason: String,
    },
}

/// Open the DB with the settings implied by `path` itself (see
/// [`parse_open_input`]) — the Go-compatible entry point. Navidrome's default DSN
/// (`consts.DefaultDbPath`, `consts/consts.go:14`) carries its own
/// `_journal_mode`/`_busy_timeout`/`_foreign_keys`, so it resolves to
/// [`DbOpenOptions::navidrome_default_options`]; any other path resolves to
/// go-sqlite3's own driver state ([`DbOpenOptions::driver_defaults`]) plus whatever
/// its own query asks for, which is what Go does — `conf/configuration.go:404-406`
/// applies `DefaultDbPath` only when `DbPath` is empty.
///
/// Every handle gets the per-connection extensions (contract §4) AND the open
/// options applied as explicit `PRAGMA` statements with the effective state read
/// back before the handle is returned ([`apply_open_options`]). Connections are
/// single-owner values threaded by the caller (explicit DI per the #S1 freeze,
/// replacing Go's process-global singleton `Db()`).
///
/// # Errors
/// [`Error::InvalidDsn`] for an unsupported/unknown DSN parameter,
/// [`Error::Failed`] when SQLite refuses the open or a pragma fails,
/// [`Error::OpenConfiguration`] when a pragma's effective state differs from
/// the request.
pub fn open(path: &str) -> Result<Connection, Error> {
    open_with_options(path, None)
}

/// Open `path` with explicit `options`, overriding what the DSN itself asks for.
/// Same guarantees as [`open`]: extensions registered, pragmas applied after
/// registration, effective state of every EXPLICIT setting read back.
///
/// `DbOpenOptions::journal_mode`/`foreign_keys` set to `None` mean "the DSN said
/// nothing", and no `PRAGMA` is emitted for them — matching go-sqlite3, which only
/// execs `PRAGMA journal_mode` when a journal was named (`sqlite3.go:1865-1867`) and
/// only execs `PRAGMA foreign_keys` when it was set (`:1851-1853`). Nothing is
/// asserted about those two in that case: an existing WAL file reports `wal` on its
/// own, and this build's FK default comes from
/// `-DSQLITE_DEFAULT_FOREIGN_KEYS=1`, neither of which a DSN that says nothing
/// should override.
///
/// # Errors
/// As [`open`].
pub fn open_with_options(path: &str, options: Option<DbOpenOptions>) -> Result<Connection, Error> {
    let target = parse_open_input(path)?;
    let want = options.unwrap_or(target.options);
    // URI-capable flags: the `:memory:` rewrite and any preserved SQLite URI
    // parameter must parse (`db/db.go:54-55`).
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_URI;
    // `vfs` goes to SQLite as open's own argument, not as part of the filename: that is
    // exactly what go-sqlite3 does (`sqlite3.go:1607-1614`), and it is why a bogus name
    // fails with SQLite's `no such vfs` for a plain path too.
    let conn = match target.vfs.as_deref() {
        Some(vfs) => Connection::open_with_flags_and_vfs(&target.sqlite_arg, flags, vfs),
        None => Connection::open_with_flags(&target.sqlite_arg, flags),
    }
    .map_err(|e| Error::Failed {
        path: path.to_string(),
        cause: e,
    })?;
    register_extensions(&conn).map_err(|e| Error::Failed {
        path: path.to_string(),
        cause: e,
    })?;
    // Extensions first, pragmas second. Go's order is the other way round — the
    // driver execs its PRAGMA list (`sqlite3.go:1665` busy timeout, `:1852`
    // foreign keys, `:1866` journal mode, `:1906` synchronous) and only then runs
    // `ConnectHook` (`:1930`). That difference is unobservable: registering a
    // collation or scalar function changes no pragma state. What matters is that
    // both complete before the handle is handed out, so a handle whose
    // configuration fails never reaches a caller.
    apply_open_options(&conn, want, target.is_memory, path)?;
    Ok(conn)
}

/// What a DB input resolves to: the argument SQLite is actually handed, the path
/// component it names, the settings it asks for, and whether it is a memory DB.
/// Produced by [`parse_open_input`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenTarget {
    /// The exact string handed to `Connection::open_with_flags`: never a filename
    /// with an appended query string.
    pub sqlite_arg: String,
    /// Path component as written (`navidrome.db`, `:memory:`, …) with the query
    /// removed. SQLite canonicalizes it to an absolute path, which is what
    /// [`database_file`] reports.
    pub path: String,
    /// Settings the input asks for (Go defaults where it says nothing).
    pub options: DbOpenOptions,
    /// True for the bare `:memory:` path and for `mode=memory` **URIs**: SQLite keeps
    /// such a DB on the `memory` journal and cannot switch it to WAL. A non-URI
    /// `x.db?mode=memory` is false, because its query is stripped the way go-sqlite3
    /// strips it and it is a real file.
    pub is_memory: bool,
    /// `vfs=`, extracted the way go-sqlite3 extracts it — before the query is stripped,
    /// so a NON-URI input carries it too — and handed to
    /// `Connection::open_with_flags_and_vfs` as SQLite's zVfs argument, exactly like
    /// Go's `sqlite3_open_v2` call (`sqlite3.go:1595-1614`). Ignoring it would silently
    /// open on the default VFS.
    pub vfs: Option<String>,
}

/// Translate a Go-style DB input into an [`OpenTarget`] using two grammars.
///
/// Go parses a query only when the first '?' is at index >= 1 (`sqlite3.go:1247`).
/// Its `url.ParseQuery` map percent-decodes names/values, maps '+' to space, and
/// retains the FIRST value per decoded name. The alias beats the canonical name
/// wherever present, including an empty alias (`:1338-1345`, `:1401-1408`,
/// `:1438-1445`, `:1538-1545`). Option names are case-sensitive, the four option
/// values case-insensitive. This map configures the driver options and zVfs ONLY.
///
/// For accepted `file:` inputs, SQLite receives the ORIGINAL URI, including every
/// query occurrence and inert underscore convention. Its decoder leaves '+'
/// literal (`sqlite3.c:182839-182870`), and its ordered option loop progressively
/// applies ALL mode/cache/vfs values (`:182903-182965`), with later values
/// overwriting earlier ones where allowed. URI vfs OVERWRITES the explicit zVfs.
/// Non-URI inputs reach SQLite as plain filenames with the query stripped
/// (`sqlite3.go:1599-1601`), except the exact bare `:memory:` rewrite and
/// Navidrome's default DSN forwarding only `cache=shared`.
///
/// Complete policy over Go's decoded first-value map (not a URI query rebuild):
///
/// | parameter | handling |
/// |---|---|
/// | `_journal_mode`/`_journal`, `_busy_timeout`/`_timeout`, `_synchronous`/`_sync`, `_foreign_keys`/`_fk` ([`GO_OPTION_KEYS`]) | resolve alias/first-value precedence into [`DbOpenOptions`]; empty winning value leaves it unset. Retained, but inert to SQLite, in a file: URI. |
/// | `_cache_size` ([`GO_OPTION_KEYS`]) | first Go-decoded value, empty means unset; otherwise signed base-10 int64 grammar, applied as `PRAGMA cache_size` and read back (`sqlite3.go:1917-1921`). |
/// | `vfs`, `cache`, `mode`, `immutable`, `nolock`, `psow`, `readonly_shm`, `exclusive`, `modeof`, `8_3_names`, `rbu_memory` ([`SUPPORTED_URI_PARAMS`]) | reject an empty FIRST Go-decoded value; otherwise SQLite interprets the untouched file: query. Non-URI queries are stripped. The first Go-decoded `vfs` is additionally passed as zVfs in BOTH shapes (`sqlite3.go:1595-1614`); URI vfs occurrences overwrite it in order. |
/// | `journal_mode`, `journal`, `busy_timeout`, `timeout`, `synchronous`, `sync`, `foreign_keys`, `fk` ([`GO_IGNORED_BARE_NAMES`]) | accepted with any value, ignored by both Go's conventions and SQLite, retained in a file: URI. |
/// | `_txlock`, `_loc` ([`GO_DRIVER_ONLY_PARAMS`]) | empty accepted; otherwise `_txlock` must be deferred/immediate/exclusive (case-insensitive), `_loc` auto or an ASCII zone-name shape. No SQLite state configured; retained but inert in a file: URI. Zone existence is deliberately not checked against host zoneinfo. |
/// | [`GO_STATE_PRAGMAS`] (`_auto_vacuum`/`_vacuum`, `_case_sensitive_like`/`_cslike`, `_defer_foreign_keys`/`_defer_fk`, `_ignore_check_constraints`, `_locking_mode`/`_locking`, `_query_only`, `_recursive_triggers`/`_rt`, `_secure_delete`, `_writable_schema`) and [`GO_DRIVER_STATE_PARAMS`] (`_mutex`, `_stmt_cache_size`, `_auth`, `_auth_user`, `_auth_pass`, `_auth_crypt`, `_auth_salt`) | resolve each canonical/alias family by presence FIRST, ignoring the loser's value entirely; empty winner configures nothing, except `_auth`, whose presence alone triggers rejection (Go also errors without `_auth_user`, wording differs). Nonempty winners undergo Go-grammar validation then rejection as unsupported. |
/// | anything else, including uppercase/misspelled names and `verbose` | [`Error::InvalidDsn`] as unknown. |
/// | percent-decoded query names/values must be UTF-8 | deliberate input restriction, including inert values (`_loc=%FF`, `journal_mode=%FF`). Go permits arbitrary bytes; the port explicitly rejects them with `not valid UTF-8`, never silently replaces them. |
///
/// Other explicit input restrictions: raw ';' (Go's ParseQuery error), malformed
/// percent escapes, bare query items without '=', and empty names are rejected.
/// Empty pairs (&&) are skipped. A '#' in a file: URI is rejected to prevent
/// fragment truncation; plain filenames retain it. SQLite itself rejects invalid
/// URI authorities (`sqlite3.c:182815-182824`).
///
/// The default DSN's forwarded shared cache is a deliberate D-OPEN divergence:
/// Go strips it. It is forwarded only if the plain path can be URI-addressed
/// (no leading '//'). The exact input ':memory:' becomes
/// `file::memory:?cache=shared` with FK on (`db/db.go:53-56`);
/// ':memory:?cache=shared' stays private, and its query is stripped like any
/// other non-URI input. 'file::memory:' remains private unless its own URI asks
/// for a shared cache. A leading '?' is part of the filename, not a query.
pub fn parse_open_input(input: &str) -> Result<OpenTarget, Error> {
    let is_uri = input.starts_with("file:");
    // SQLite terminates a `file:` URI path at '#' (`sqlite3.c:182839-182874`), so a
    // fragment silently truncates the filename — measured:
    // `file:…/hash#name.db` opens `…/hash`. A plain (non-URI) filename is not
    // parsed that way and keeps its '#' verbatim, which is also what Go's stripped
    // DSN does, so only the URI form is rejected here.
    if is_uri && input.contains('#') {
        return Err(invalid_dsn(
            input,
            "a '#' in a file: URI truncates the database name in SQLite; percent-encode it as %23",
        ));
    }
    let (head, raw_query) = match input.find('?') {
        Some(at) if at >= 1 => (&input[..at], Some(&input[at + 1..])),
        _ => (input, None),
    };
    // go-sqlite3 hands the query to `url.ParseQuery` and the resulting statement
    // list to SQLite; a raw `;` in the query makes the FIRST statement fail with
    // `invalid semicolon separator in query` (measured, v1.14.52 — every position
    // fails: leading, trailing, mid-item, inside a value, and `;;`). Refuse it up
    // front. A percent-encoded `%3B` is NOT a separator: it decodes into the value,
    // and Go then fails on that value (`Invalid _foreign_keys: on;_journal_mode=wal`),
    // which this port reproduces through value validation.
    if let Some(text) = raw_query {
        if text.contains(';') {
            return Err(invalid_dsn(
                input,
                "invalid semicolon separator in query (go-sqlite3 rejects it on first use; percent-encode a literal ';' as %3B)",
            ));
        }
    }
    let written_path = if is_uri {
        head.strip_prefix("file:").unwrap_or(head).to_string()
    } else {
        head.to_string()
    };
    // A path that would give a `file:` URI an authority (`file://tmp/x.db`) is
    // rejected by SQLite's own URI parser (`sqlite3.c:182815-182824`, "invalid uri
    // authority"), but go-sqlite3 never builds a URI for a non-`file:` DSN: it strips
    // the query (`sqlite3.go:1599-1601`) and hands the string to `sqlite3_open_v2` as
    // a plain filename, so `//tmp/x.db?<query>` opens `/tmp/x.db` (measured:
    // WAL/15000/FK-on with `database_list` = `/private/tmp/…/navidrome.db`). An input
    // whose path starts with `//` therefore stays a PLAIN filename here too — and a
    // plain filename cannot carry SQLite URI parameters, which is Go's behavior.
    let sqlite_can_address_path = !written_path.starts_with("//");

    // Parse the query the way go-sqlite3 does: `url.ParseQuery` percent-decodes
    // names and values, so `_journal%5Fmode=WAL` and `%5Fjournal_mode=WAL` are both
    // `_journal_mode=WAL` (measured in Go), and a malformed `%` escape is an error
    // (`invalid URL escape`) rather than a silently mangled name.
    let params = parse_go_query(input, raw_query)?;

    // The four settings, resolved with go-sqlite3's precedence. Each option block
    // resets `pkey` to `""`, sets it to the canonical name if that key is PRESENT,
    // then overwrites it with the alias name if the alias key is present
    // (`sqlite3.go:1338-1345`, `:1401-1408`, `:1438-1445`, `:1538-1545`), so the
    // LAST name present wins — the alias beats the canonical wherever the alias
    // appears, whatever its value — and `params.Get(pkey)` then returns that key's
    // FIRST value. Measured against the driver:
    //   ?_busy_timeout=111&_timeout=222    -> 222   (and 222 in the other order)
    //   ?_journal_mode=wal&_journal=delete -> delete
    //   ?_journal_mode=wal&_journal=       -> delete (alias present, empty value => unset)
    //   ?_busy_timeout=111&_busy_timeout=222 -> 111 (first occurrence of the winner)
    //   ?_foreign_keys=off&_fk=on          -> fk=1
    let journal_value = go_option_value(&params, Journal::CANONICAL, Journal::ALIAS);
    let busy_value = go_option_value(&params, Busy::CANONICAL, Busy::ALIAS);
    let sync_value = go_option_value(&params, Sync::CANONICAL, Sync::ALIAS);
    let fk_value = go_option_value(&params, Fk::CANONICAL, Fk::ALIAS);
    let cache_value = go_option_value(&params, "_cache_size", "_cache_size");

    let mut journal: Option<JournalMode> = None;
    let mut busy_timeout: Option<u64> = None;
    let mut synchronous: Option<Synchronous> = None;
    let mut foreign_keys: Option<bool> = None;
    let mut cache_size: Option<i64> = None;
    if let Some((name, value)) = &journal_value {
        if !value.is_empty() {
            journal = Some(parse_journal(input, name, value)?);
        }
    }
    if let Some((name, value)) = &busy_value {
        if !value.is_empty() {
            busy_timeout = Some(parse_busy_timeout(input, name, value)?);
        }
    }
    if let Some((name, value)) = &sync_value {
        if !value.is_empty() {
            synchronous = Some(parse_synchronous(input, name, value)?);
        }
    }
    if let Some((name, value)) = &fk_value {
        if !value.is_empty() {
            foreign_keys = Some(parse_boolean(input, name, value)?);
        }
    }
    if let Some((name, value)) = &cache_value {
        if !value.is_empty() {
            cache_size = Some(value.parse::<i64>().map_err(|_| {
                invalid_dsn(
                    input,
                    &format!("invalid {name}: '{value}', expecting an int"),
                )
            })?);
        }
    }

    // Everything that is not one of the five settings. go-sqlite3 recognizes a fixed
    // set beyond them, SQLite recognizes a fixed set of URI options, and anything
    // outside both is refused rather than dropped — see the policy table on
    // [`parse_open_input`].
    let mut vfs: Option<String> = None;
    for (name, value) in &params {
        if go_option_class(name).is_some() {
            continue;
        }
        // Spelled without the underscore: go-sqlite3 ignores these outright.
        if GO_IGNORED_BARE_NAMES.contains(&name.as_str()) {
            continue;
        }
        // Driver-side only (`_txlock`, `_loc`): no SQLite state at all, so
        // the value is validated like Go's and the parameter is dropped.
        if GO_DRIVER_ONLY_PARAMS.contains(&name.as_str()) {
            validate_go_driver_only(input, name, value)?;
            continue;
        }
        if GO_STATE_PRAGMAS.contains(&name.as_str())
            || GO_DRIVER_STATE_PARAMS.contains(&name.as_str())
        {
            // Resolve EACH unsupported alias family by presence before validating.
            // A loser's value is never inspected, even if it is invalid/nonempty.
            if let Some((canonical, alias)) = GO_STATE_ALIASES
                .iter()
                .find(|(canonical, alias)| name == canonical || name == alias)
            {
                let (winner, _) = go_option_value(&params, canonical, alias)
                    .expect("the current name belongs to this family");
                if *name != winner {
                    continue;
                }
            }
            // Empty Get values configure nothing; `_auth` alone tests presence.
            if value.is_empty() && name != "_auth" {
                continue;
            }
            // Validate first, exactly as the driver does, so a value Go would reject
            // is rejected here too — then refuse the parameter, because rusqlite has
            // no per-handle equivalent and accepting it would hand out a handle whose
            // setting was silently ignored.
            validate_go_state_option(input, name, value)?;
            return Err(invalid_dsn(
                input,
                &format!(
                    "go-sqlite3 parameter '{name}={value}' is well-formed but cannot be applied by this port: rusqlite 0.32.1 has no per-handle equivalent for it, and accepting it would hand out a handle whose setting was silently ignored"
                ),
            ));
        }
        if !SUPPORTED_URI_PARAMS.contains(&name.as_str()) {
            return Err(invalid_dsn(
                input,
                &format!(
                    "unknown parameter '{name}': supported SQLite parameters are {}; go-sqlite3 conventions mapped to open options are {}; go-sqlite3 options this port accepts and ignores are {}; go-sqlite3 options it refuses are {}",
                    SUPPORTED_URI_PARAMS.join(", "),
                    GO_OPTION_KEYS.join(", "),
                    GO_IGNORED_BARE_NAMES.join(", "),
                    GO_REJECTED_PARAMS.join(", ")
                ),
            ));
        }
        if value.is_empty() {
            return Err(invalid_dsn(
                input,
                &format!("SQLite URI parameter '{name}' has an empty value"),
            ));
        }
        if name == "vfs" {
            // go-sqlite3 pulls `vfs` out of the DSN BEFORE stripping the query
            // (`sqlite3.go:1595-1597`) and passes it as its own argument to
            // `sqlite3_open_v2` (`:1607-1614`), so it is honored for a NON-URI DSN too,
            // and a bad name fails there with `no such vfs: not-a-vfs` (measured).
            // rusqlite exposes the same argument — `Connection::open_with_flags_and_vfs`
            // (`rusqlite-0.32.1/src/lib.rs:492-500`, which forwards `Some(&c_vfs)` to
            // `sqlite3_open_v2` at `src/inner_connection.rs:69-88`) — so this port hands
            // it over instead of ignoring it. For URI inputs SQLite OVERWRITES zVfs
            // with each URI `vfs=` in order (`sqlite3.c:182903-182908`); the raw
            // query below must therefore remain untouched, even with duplicates.
            vfs = Some(value.clone());
        }
    }

    // `:memory:` is Go's rewrite at `db/db.go:54-55`, whose test is the exact
    // equality `Path == ":memory:"` — no query, no `file:` prefix — and which appends
    // BOTH `cache=shared` and `_foreign_keys=on`. Only `cache=shared` is a SQLite URI
    // parameter; the FK convention is folded into the options.
    //
    // The boundary matters (`db/db.go:53-56` is exact equality on the path):
    // `file::memory:` and `:memory:?_fk=off` do NOT match it, so Go leaves them as the
    // caller wrote them and they stay PRIVATE (measured with two handles, the second
    // gets `no such table`). Only the bare `:memory:` becomes shared, and only that
    // path gets `cache=shared` injected or `_foreign_keys` defaulted on.
    // Infer memory only for the initial journal expectation. SQLite remains the
    // authority: its ordered mode loop may change MEMORY back to a file open.
    let uri_mode_memory = is_uri
        && raw_query
            .into_iter()
            .flat_map(|query| query.split('&'))
            .filter_map(|item| item.split_once('='))
            .filter_map(|(name, value)| {
                let name = percent_decode(input, name).ok()?;
                (name == "mode")
                    .then(|| percent_decode(input, value).ok())
                    .flatten()
            })
            .next_back()
            .is_some_and(|mode| mode.split('\0').next() == Some("memory"));
    let memory_path = if is_uri {
        // sqlite3ParseUri discards the rest of this component after decoded NUL
        // (`sqlite3.c:182849-182863`); classification only, never rewrite the URI.
        percent_decode(input, &written_path)
            .is_ok_and(|path| path.split('\0').next() == Some(MEMORY_PATH))
    } else {
        written_path == MEMORY_PATH
    };
    let is_memory = memory_path || uri_mode_memory;
    if is_naked_memory_path(input) {
        foreign_keys = Some(true);
    }

    // Base state: go-sqlite3's driver defaults, or the memory journal SQLite keeps
    // for a memory DB (`sqlite3.c:64434-64436` refuses WAL there; measured: a
    // memory handle reports `memory` even when asked for WAL).
    let mut options = if is_memory {
        DbOpenOptions::driver_defaults().with_journal(JournalMode::Memory)
    } else {
        DbOpenOptions::driver_defaults()
    };
    // Navidrome's preset is a config default, not a driver default:
    // `conf/configuration.go:404-406` substitutes `consts.DefaultDbPath` only when
    // `DbPath` is empty, so this exact DSN is the only input that gets WAL/15000/
    // NORMAL/FK-on without saying so in its query.
    if is_navidrome_default_dsn(&written_path, raw_query) {
        options = DbOpenOptions::navidrome_default_options();
    }
    if let Some(value) = journal {
        options.journal_mode = Some(value);
    }
    if let Some(value) = busy_timeout {
        options.busy_timeout = value;
    }
    if let Some(value) = synchronous {
        options.synchronous = value;
    }
    if let Some(value) = foreign_keys {
        options.foreign_keys = Some(value);
    }
    options.cache_size = cache_size;

    // TWO grammars: Go extracts conventions/zVfs using ParseQuery above, but
    // hands SQLite the ORIGINAL file: URI (`sqlite3.go:1595-1614`). SQLite leaves
    // '+' literal, percent-decodes once (`sqlite3.c:182839-182870`), and applies
    // ALL mode/cache/vfs occurrences progressively (`:182903-182965`). Never
    // rebuild this query from Go's decoded, first-value map.
    let sqlite_arg = if is_uri {
        input.to_string()
    } else if is_naked_memory_path(input) {
        "file::memory:?cache=shared".to_string()
    } else if is_navidrome_default_dsn(&written_path, raw_query) && sqlite_can_address_path {
        format!(
            "file:{}?cache=shared",
            percent_encode_uri_path(&written_path)
        )
    } else {
        // Non-URI queries are stripped, INCLUDING :memory:?cache=shared.
        written_path.clone()
    };

    Ok(OpenTarget {
        sqlite_arg,
        path: written_path,
        options,
        is_memory,
        vfs,
    })
}

/// True only for the input go-sqlite3's `:memory:` rewrite fires on: the exact
/// string `:memory:`, with no query and no `file:` prefix (`db/db.go:53-55`).
fn is_naked_memory_path(input: &str) -> bool {
    input == MEMORY_PATH
}

/// One go-sqlite3 option class: canonical key first, then the alias key
/// (`sqlite3.go:1339-1344` and friends).
trait GoOptionClass {
    const CANONICAL: &'static str;
    const ALIAS: &'static str;
}
struct Journal;
impl GoOptionClass for Journal {
    const CANONICAL: &'static str = "_journal_mode";
    const ALIAS: &'static str = "_journal";
}
struct Busy;
impl GoOptionClass for Busy {
    const CANONICAL: &'static str = "_busy_timeout";
    const ALIAS: &'static str = "_timeout";
}
struct Sync;
impl GoOptionClass for Sync {
    const CANONICAL: &'static str = "_synchronous";
    const ALIAS: &'static str = "_sync";
}
struct Fk;
impl GoOptionClass for Fk {
    const CANONICAL: &'static str = "_foreign_keys";
    const ALIAS: &'static str = "_fk";
}

/// go-sqlite3's `pkey` resolution: the canonical name if present, then the alias
/// name if present (so the alias wins when both appear), and `params.Get` giving
/// that key's first value. Returns `(key, value)` so the error message names the
/// key that was actually read.
fn go_option_value(
    params: &[(String, String)],
    canonical: &str,
    alias: &str,
) -> Option<(String, String)> {
    let mut pkey: Option<&str> = None;
    if params.iter().any(|(name, _)| name == canonical) {
        pkey = Some(canonical);
    }
    if params.iter().any(|(name, _)| name == alias) {
        pkey = Some(alias);
    }
    let key = pkey?;
    params
        .iter()
        .find(|(name, _)| name == key)
        .map(|(name, value)| (name.clone(), value.clone()))
}

/// `url.ParseQuery` semantics over the `name=value` grammar this port accepts:
/// percent-decoded names and values, `+` meaning a space, a repeated name keeping
/// its first value, empty pairs (`&&`) skipped. A malformed `%` escape is rejected
/// ([`Error::InvalidDsn`]) — Go's `url.ParseQuery` errors there too, but only
/// surfaces it on first use.
///
/// Two inputs Go accepts are rejected here, both because the port refuses to apply
/// a setting nobody can name: a bare item with no `=` (`?cache`, which
/// `url.ParseQuery` stores under an empty value and which no option then reads),
/// and an empty parameter name (`?=wal`), which in go-sqlite3 ends up as
/// `params.Get("")` — the FIRST empty-named pair — so ConnectHook fails on first
/// use with `Invalid _auto_vacuum: wal` (measured). An empty VALUE is rejected for
/// every kept URI parameter, even though SQLite treats an empty value as false for
/// its boolean options (`nolock=`, `immutable=` open with the flag off, measured);
/// requiring an explicit value keeps the request unambiguous.
fn parse_go_query(input: &str, query: Option<&str>) -> Result<Vec<(String, String)>, Error> {
    let Some(text) = query else {
        return Ok(Vec::new());
    };
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let mut params: Vec<(String, String)> = Vec::new();
    for item in text.split('&') {
        if item.is_empty() {
            continue;
        }
        let (raw_name, raw_value) = match item.find('=') {
            Some(at) => (&item[..at], &item[at + 1..]),
            None => {
                return Err(invalid_dsn(
                    input,
                    &format!("parameter '{item}' has no value (expected name=value)"),
                ));
            }
        };
        if raw_name.is_empty() {
            return Err(invalid_dsn(
                input,
                "an empty parameter name is not accepted",
            ));
        }
        let name = percent_decode(input, raw_name)?;
        let value = percent_decode(input, raw_value)?;
        if name.is_empty() {
            return Err(invalid_dsn(
                input,
                "an empty parameter name is not accepted",
            ));
        }
        if !params.iter().any(|(seen, _)| seen == &name) {
            params.push((name, value));
        }
    }
    Ok(params)
}

/// Percent-decoding over UTF-8 bytes: `%HH` for any byte, `+` for a space. Go's
/// `url.QueryUnescape` does the same on UTF-8 strings, and go-sqlite3 matches the
/// DECODED name — measured: `?_journal%5Fmode=WAL` and `?%5Fjournal_mode=WAL` both
/// give journal=wal, so a raw split on `&`/`=` without decoding is not
/// Go-compatible.
///
/// SQLite's separate URI decoder (`sqlite3.c:182839-182870`) leaves '+' literal.
/// This function is ONLY Go's query decoder; its map is never re-emitted to SQLite.
/// Deliberate port restriction: percent-decoded query names/values must be UTF-8,
/// including inert options such as `_loc`. Go permits arbitrary bytes (`%FF`);
/// this port rejects them explicitly rather than replacing bytes or changing paths.
fn percent_decode(input: &str, text: &str) -> Result<String, Error> {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut at = 0usize;
    while at < bytes.len() {
        match bytes[at] {
            b'%' => {
                if at + 2 >= bytes.len() {
                    return Err(invalid_dsn(
                        input,
                        &format!(
                            "invalid URL escape in '{text}': '%' is not followed by two hex digits"
                        ),
                    ));
                }
                let hi = bytes[at + 1];
                let lo = bytes[at + 2];
                match (hi.checked_as_hex(), lo.checked_as_hex()) {
                    (Some(hi), Some(lo)) => {
                        out.push(hi * 16 + lo);
                        at += 3;
                    }
                    _ => {
                        return Err(invalid_dsn(
                            input,
                            &format!(
                                "invalid URL escape '%{}{}' in '{text}'",
                                char::from(hi),
                                char::from(lo)
                            ),
                        ));
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                at += 1;
            }
            other => {
                out.push(other);
                at += 1;
            }
        }
    }
    match String::from_utf8(out) {
        Ok(text) => Ok(text),
        Err(_invalid) => Err(invalid_dsn(
            input,
            &format!("'{text}' decodes to bytes that are not valid UTF-8"),
        )),
    }
}

/// Two hex digits of a `%HH` escape.
trait AsHex {
    fn checked_as_hex(self) -> Option<u8>;
}
impl AsHex for u8 {
    fn checked_as_hex(self) -> Option<u8> {
        match self {
            b'0'..=b'9' => Some(self - b'0'),
            b'a'..=b'f' => Some(self - b'a' + 10),
            b'A'..=b'F' => Some(self - b'A' + 10),
            _ => None,
        }
    }
}

/// Validate a go-sqlite3 option that configures no SQLite connection state, with the
/// driver's own grammar: `_txlock` is matched case-insensitively against
/// `deferred`/`immediate`/`exclusive` (`sqlite3.go:1296-1308`, error site `:1306`
/// `Invalid _txlock: %v`), and `_loc` accepts the literal `auto` plus anything
/// `time.LoadLocation` can load (`:1271-1282`, error site `:1279`
/// `Invalid _loc: %v: %v`). An
/// empty value is skipped by Go's `params.Get(val != "")` guard and configures
/// nothing, so it is accepted here too.
fn validate_go_driver_only(input: &str, name: &str, value: &str) -> Result<(), Error> {
    if value.is_empty() {
        return Ok(());
    }
    match name {
        "_txlock" => match value.to_lowercase().as_str() {
            "deferred" | "immediate" | "exclusive" => Ok(()),
            _ => Err(invalid_dsn(
                input,
                &format!("invalid _txlock: '{value}', expecting 'deferred immediate exclusive'"),
            )),
        },
        // Go resolves "auto" to time.Local without touching zoneinfo; anything else
        // goes through time.LoadLocation (`sqlite3.go:1271-1282`). The port cannot
        // consult the host's zone database, so it accepts the SHAPE Go's loader
        // requires (letters/digits/`+`/`-`/`_`/`/`, e.g. `UTC`, `Local`, `Asia/Tokyo`)
        // and rejects a malformed one, which is what Go's measured
        // `Invalid _loc: invalid: unknown time zone invalid` refuses too. A
        // well-formed name that is simply not installed (measured:
        // `_loc=UTC/Paris` -> `Invalid _loc: UTC/Paris: not a directory`) is accepted
        // here — the only divergence, and it cannot change SQLite connection state,
        // since `_loc` configures nothing on the handle.
        "_loc" => {
            if value.eq_ignore_ascii_case("auto") {
                return Ok(());
            }
            let shaped = value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '_' | '/' | ':'));
            if shaped {
                Ok(())
            } else {
                Err(invalid_dsn(
                    input,
                    &format!("invalid _loc: '{value}': unknown time zone"),
                ))
            }
        }
        other => Err(invalid_dsn(
            input,
            &format!("{other} is not a driver-only go-sqlite3 option"),
        )),
    }
}

/// Validate a go-sqlite3 parameter whose value the driver checks, using the driver's
/// own grammar (v1.14.52 `sqlite3.go:1254-1593`). The parameter is refused
/// afterwards because the port cannot apply it, but validating first means the port
/// never accepts a DSN that go-sqlite3 would reject.
fn validate_go_state_option(input: &str, name: &str, value: &str) -> Result<(), Error> {
    // An empty value makes go-sqlite3 skip its whole block (`params.Get(pkey)` with
    // `val != ""`), so it provably configures nothing; `_auth` is the exception —
    // presence alone triggers it (`sqlite3.go:1254-1256`) — so it is refused
    // whatever its value.
    if value.is_empty() && name != "_auth" {
        return Ok(());
    }
    // go-sqlite3 reports the CANONICAL name in its error even when the alias was
    // written (`Invalid _auto_vacuum: ...` for `_vacuum=x`, `sqlite3.go:1330`), so
    // the messages below use the canonical spelling too.
    let canonical = match name {
        "_vacuum" => "_auto_vacuum",
        "_cslike" => "_case_sensitive_like",
        "_defer_fk" => "_defer_foreign_keys",
        "_locking" => "_locking_mode",
        "_rt" => "_recursive_triggers",
        other => other,
    };
    match name {
        "_auto_vacuum" | "_vacuum" => match value.to_lowercase().as_str() {
            "0" | "none" | "1" | "full" | "2" | "incremental" => Ok(()),
            _ => Err(invalid_dsn(
                input,
                &format!("invalid {canonical}: '{value}', expecting value of '0 NONE 1 FULL 2 INCREMENTAL'"),
            )),
        },
        "_locking_mode" | "_locking" => match value.to_uppercase().as_str() {
            "NORMAL" | "EXCLUSIVE" => Ok(()),
            _ => Err(invalid_dsn(
                input,
                &format!("invalid {canonical}: '{value}', expecting value of 'NORMAL EXCLUSIVE'"),
            )),
        },
        "_secure_delete" => match value.to_lowercase().as_str() {
            "0" | "no" | "false" | "off" | "1" | "yes" | "true" | "on" | "fast" => Ok(()),
            _ => Err(invalid_dsn(
                input,
                &format!("invalid {canonical}: '{value}', expecting boolean value of '0 1 false true no yes off on fast'"),
            )),
        },
        "_stmt_cache_size" => match value.parse::<i64>() {
            Ok(n) if n >= 0 => Ok(()),
            Ok(_negative) => Err(invalid_dsn(
                input,
                &format!("invalid {canonical}: '{value}', expecting non-negative integer"),
            )),
            Err(_cause) => Err(invalid_dsn(
                input,
                &format!("invalid {canonical}: '{value}', expecting an int"),
            )),
        },
        // Presence-only, no value grammar (`if _, ok := params["_auth"]; ok`).
        "_auth" => Ok(()),
        // Plain strings taken verbatim (`sqlite3.go:1257-1268`).
        "_auth_user" | "_auth_pass" | "_auth_crypt" | "_auth_salt" => Ok(()),
        "_mutex" => match value.to_lowercase().as_str() {
            "no" | "full" => Ok(()),
            _ => Err(invalid_dsn(input, &format!("invalid {canonical}: '{value}'"))),
        },
        "_query_only"
        | "_case_sensitive_like"
        | "_cslike"
        | "_defer_foreign_keys"
        | "_defer_fk"
        | "_ignore_check_constraints"
        | "_recursive_triggers"
        | "_rt"
        | "_writable_schema" => {
            if gosql_boolean(value) {
                Ok(())
            } else {
                Err(invalid_dsn(
                    input,
                    &format!("invalid {canonical}: '{value}', expecting boolean value of '0 1 false true no yes off on'"),
                ))
            }
        }
        other => Err(invalid_dsn(
            input,
            &format!("{other} has no validated go-sqlite3 grammar"),
        )),
    }
}

/// go-sqlite3's boolean list, matched with `strings.ToLower`
/// (`sqlite3.go:1408-1416` and the identical blocks for the pragma options).
fn gosql_boolean(value: &str) -> bool {
    matches!(
        value.to_lowercase().as_str(),
        "0" | "no" | "false" | "off" | "1" | "yes" | "true" | "on"
    )
}

/// Byte-level percent-encoding of a path for re-emitting it inside a `file:` URI.
/// SQLite decodes `%HH` while parsing the URI (`sqlite3.c:182839-182870`), so a
/// name containing `%`, `?`, `#`, a space or `&` must be encoded or SQLite opens a
/// different file (measured: `file:…/%20lit.db` opens `…/ lit.db`, while
/// `%2520lit.db` opens the literal `%20lit.db`). `/` stays literal. A `file:` input
/// is never re-encoded — that would double-decode what the caller already encoded.
fn percent_encode_uri_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                out.push(char::from(*byte));
            }
            other => out.push_str(&format!("%{:02X}", *other)),
        }
    }
    out
}

/// True when the input is Navidrome's own default DSN: `nd-conf`'s
/// `DEFAULT_DB_PATH` (`nd-conf/src/consts.rs:16`) itself, or a data folder joined to
/// its basename with the same parameter set (`Config::normalize_and_validate`,
/// `nd-conf/src/config.rs:999-1010`, mirrors `conf/configuration.go:404-406`).
///
/// The comparison runs on the DECODED, order-insensitive parameter set — the same
/// grammar as [`parse_go_query`] — so the percent-encoded spelling of the canonical
/// query,
/// `navidrome.db?cache=shared&_busy%5Ftimeout=15000&_journal%5Fmode=WAL&_foreign%5Fkeys=on&synchronous=normal`,
/// counts too. Measured against go-sqlite3, that form gives byte-identical pragma
/// state to the plain one, and matching on raw items used to miss it: the preset was
/// not recognized, `cache=shared` was dropped from the SQLite argument, and the
/// handle ended up with `SQLITE_LOCKED_SHAREDCACHE` (extended 262) instead of
/// `SQLITE_BUSY` (extended 5) — a different lock class for the same configuration.
///
/// Caveat, and a documented divergence from Go: matching is on basename + parameter
/// SET, not on the configured path, so a user-supplied `/data/navidrome.db` carrying
/// the same query matches as well and gets WAL/15000/NORMAL/FK-on plus forwarded
/// `cache=shared`, where go-sqlite3 would give WAL/15000/FK-on without a shared
/// cache. `Config::normalize_and_validate` produces exactly that shape whenever
/// `DbPath` is unset, which is why the preset is a DSN-shape test rather than a
/// path test.
fn is_navidrome_default_dsn(path: &str, query: Option<&str>) -> bool {
    let basename = match path.rsplit_once('/') {
        Some((_, tail)) => tail,
        None => path,
    };
    let (basename_want, query_want) = default_db_parts();
    if basename != basename_want {
        return false;
    }
    let Some(text) = query else {
        return false;
    };
    let got = decoded_param_set(text);
    let want = decoded_param_set(query_want);
    got.is_some() && got == want
}

/// Order-insensitive set of `name=value` pairs after percent-decoding, i.e. what
/// go-sqlite3's `params` map actually holds. `None` when the text cannot be decoded,
/// which never matches the preset.
fn decoded_param_set(text: &str) -> Option<Vec<(String, String)>> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for item in text.split('&') {
        if item.is_empty() {
            continue;
        }
        let (raw_name, raw_value) = item.split_once('=')?;
        let name = percent_decode_lossy(raw_name);
        let value = percent_decode_lossy(raw_value);
        pairs.push((name, value));
    }
    pairs.sort();
    Some(pairs)
}

/// [`percent_decode`] without an error channel: an undecodable escape simply keeps
/// its literal text, which cannot equal the preset's decoded form and so fails to
/// match rather than mis-applying settings.
fn percent_decode_lossy(text: &str) -> String {
    match percent_decode("", text) {
        Ok(decoded) => decoded,
        Err(_invalid) => text.to_string(),
    }
}

/// The four per-connection settings go-sqlite3's driver can configure, applied as
/// explicit `PRAGMA` statements. rusqlite has no DSN pragma layer at all (D-OPEN
/// probe), so these are the only way to reproduce Go's effective connection state.
///
/// `journal_mode` and `foreign_keys` are `Option` because "the DSN said nothing" is
/// a real state that must be expressible: go-sqlite3 execs `PRAGMA journal_mode`
/// only when a journal was named (`sqlite3.go:1865-1867`) and `PRAGMA foreign_keys`
/// only when it was set (`:1851-1853`, `foreignKeys` starts at `-1`, `:1234`). An
/// unset field emits no pragma and is not verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DbOpenOptions {
    /// `PRAGMA journal_mode`, or `None` for "not named" — SQLite then keeps its own
    /// journal (`delete` for a fresh file, `wal` for a file already in WAL).
    pub journal_mode: Option<JournalMode>,
    /// `PRAGMA busy_timeout`, milliseconds.
    pub busy_timeout: u64,
    /// `PRAGMA synchronous`.
    pub synchronous: Synchronous,
    /// `PRAGMA foreign_keys`, or `None` for "not set" (Go leaves it alone; this
    /// build's SQLite then uses `-DSQLITE_DEFAULT_FOREIGN_KEYS=1`).
    pub foreign_keys: Option<bool>,
    /// `PRAGMA cache_size` (`sqlite3.go:1917-1921`), or no pragma when unset.
    pub cache_size: Option<i64>,
}

impl DbOpenOptions {
    /// go-sqlite3's OWN driver state, i.e. what a handle gets when the DSN names
    /// nothing: journal mode unset (`journalMode` empty, `sqlite3.go:1236`),
    /// `busy_timeout = 5000` (`:1231`), `synchronous = NORMAL` always exec'd
    /// (`:1241`, `:1906`), foreign keys unset (`foreignKeys = -1`, `:1234`).
    /// Measured against the driver, a bare path gives `journal=delete`,
    /// `busy=5000`, `sync=1`, `fk=0`. This is the base state for every input that
    /// is not Navidrome's default DSN — including a user-configured custom
    /// `DbPath`, since `conf/configuration.go:404-406` applies `DefaultDbPath`
    /// only when `DbPath` is empty.
    #[must_use]
    pub fn driver_defaults() -> Self {
        Self {
            journal_mode: None,
            busy_timeout: DRIVER_BUSY_TIMEOUT_MS,
            synchronous: Synchronous::Normal,
            foreign_keys: None,
            cache_size: None,
        }
    }

    /// Navidrome's PRESET for its default DB path — WAL / 15000 / NORMAL / FK-on.
    /// These come from `consts.DefaultDbPath` (`consts/consts.go:14`, mirrored at
    /// `nd-conf/src/consts.rs:16`), which `conf/configuration.go:404-406` installs
    /// only when `DbPath` is empty; they are NOT go-sqlite3 driver defaults (see
    /// [`DbOpenOptions::driver_defaults`]). Measured against the driver, this DSN
    /// gives `journal=wal`, `busy=15000`, `sync=1`, `fk=1`.
    #[must_use]
    pub fn navidrome_default_options() -> Self {
        Self {
            journal_mode: Some(JournalMode::Wal),
            busy_timeout: NAVIDROME_BUSY_TIMEOUT_MS,
            synchronous: Synchronous::Normal,
            foreign_keys: Some(true),
            cache_size: None,
        }
    }

    /// Same options with an explicit journal mode. Used for the memory-DB journal,
    /// which SQLite fixes at `memory` (`sqlite3.c:64434-64436`; measured — a memory
    /// handle reports `memory` even when asked for WAL).
    #[must_use]
    pub fn with_journal(mut self, mode: JournalMode) -> Self {
        self.journal_mode = Some(mode);
        self
    }
}

/// `PRAGMA journal_mode` values go-sqlite3 accepts for `_journal_mode`
/// (v1.14.52 `sqlite3.go:1434-1458`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalMode {
    /// `DELETE`.
    Delete,
    /// `TRUNCATE`.
    Truncate,
    /// `PERSIST`.
    Persist,
    /// `MEMORY` — the journal a memory DB is kept on (`OFF` is the other one it
    /// accepts).
    Memory,
    /// `WAL`.
    Wal,
    /// `OFF`.
    Off,
}

impl JournalMode {
    /// The name SQLite's `PRAGMA journal_mode` reports for this mode.
    #[must_use]
    pub fn sqlite_name(self) -> &'static str {
        match self {
            JournalMode::Delete => "delete",
            JournalMode::Truncate => "truncate",
            JournalMode::Persist => "persist",
            JournalMode::Memory => "memory",
            JournalMode::Wal => "wal",
            JournalMode::Off => "off",
        }
    }
}

/// `PRAGMA synchronous` values go-sqlite3 accepts for `synchronous`
/// (v1.14.52 `sqlite3.go:1534-1552`), with the numeric code SQLite reads back.
///
/// go-sqlite3's journal block also sets `synchronousMode = "NORMAL"` when the
/// journal is WAL (`sqlite3.go:1448-1454`), which reads like a coupling, but it is
/// a NO-OP and this port deliberately does NOT reproduce it: `NORMAL` is already
/// the value of `synchronousMode` when parsing begins (`sqlite3.go:1241`), and the
/// synchronous block runs afterwards and overwrites it whenever
/// `_synchronous`/`_sync` is present (`sqlite3.go:1545-1549`). Measured against the
/// Go driver (go-sqlite3 v1.14.52, `sql.Open` + `SELECT 1` + pragma read-back):
/// `?_journal_mode=wal` -> sync=1, `?_journal_mode=wal&_synchronous=FULL` -> sync=2,
/// `_sync=3` -> sync=3. No coupling is needed to match any of those.
///
/// Note the spelling: Go reads only `_synchronous`/`_sync`, and this port reads only
/// those two as well — the bare `synchronous=` in `consts.DefaultDbPath` is ignored on
/// both sides ([`GO_IGNORED_BARE_NAMES`]), so Navidrome's effective value is 1 because
/// NORMAL is go-sqlite3's own default (`sqlite3.go:1241`) and the port's driver default
/// matches it. Measured on both sides: `file:…?synchronous=full` gives sync=1 in Go and
/// sync=1 here, and only `_synchronous=FULL`/`_sync=2` reaches 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Synchronous {
    /// `OFF` (0).
    Off,
    /// `NORMAL` (1) — go-sqlite3's own default (`sqlite3.go:1241`) and Navidrome's
    /// DSN value (`consts.DefaultDbPath` `synchronous=normal`).
    Normal,
    /// `FULL` (2) — SQLite's compile-time default (`DEFAULT_SYNCHRONOUS=2` in
    /// `PRAGMA compile_options`), i.e. what a handle that never sets the pragma
    /// reports. Go reaches it through `_synchronous`/`_sync`
    /// (`sqlite3.go:1545-1549`), which beat the WAL branch; measured against the Go
    /// driver, `?_journal_mode=wal&_synchronous=FULL` ends at sync=2.
    Full,
    /// `EXTRA` (3).
    Extra,
}

impl Synchronous {
    /// The numeric code SQLite reports for this mode.
    #[must_use]
    pub fn code(self) -> i64 {
        match self {
            Synchronous::Off => 0,
            Synchronous::Normal => 1,
            Synchronous::Full => 2,
            Synchronous::Extra => 3,
        }
    }
}

/// go-sqlite3's starting `busyTimeout` (`sqlite3.go:1231`), which the driver execs
/// as `PRAGMA busy_timeout` on every handle (`:1665`). rusqlite installs the same
/// 5000 on open (`rusqlite/src/inner_connection.rs:119`), so an unset option needs
/// no pragma to match Go.
pub const DRIVER_BUSY_TIMEOUT_MS: u64 = 5000;

/// `busy_timeout` of Navidrome's default DSN (`consts.DefaultDbPath`,
/// `consts/consts.go:14`). It has to be applied explicitly here, because neither
/// go-sqlite3's nor rusqlite's starting value is 15000.
pub const NAVIDROME_BUSY_TIMEOUT_MS: u64 = 15000;

/// go-sqlite3 DSN keys mapped onto the five [`DbOpenOptions`] settings.
/// Four have aliases; `_cache_size` has only its canonical spelling.
/// (v1.14.52: `_busy_timeout`/`_timeout` `:1339-1344`, `_foreign_keys`/`_fk`
/// `:1402-1407`, `_journal_mode`/`_journal` `:1439-1444`,
/// `_synchronous`/`_sync` `:1539-1544`); the canonical is listed first. Note the
/// underscore: the bare `synchronous`/`journal_mode`/`busy_timeout`/`foreign_keys`
/// spellings are SQLite pragma names, not go-sqlite3 options, and Go ignores them
/// ([`GO_IGNORED_BARE_NAMES`]).
pub const GO_OPTION_KEYS: &[&str] = &[
    "_journal_mode",
    "_journal",
    "_busy_timeout",
    "_timeout",
    "_synchronous",
    "_sync",
    "_foreign_keys",
    "_fk",
    "_cache_size",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GoOption {
    Journal,
    BusyTimeout,
    Synchronous,
    ForeignKeys,
    CacheSize,
}

/// Which option class a decoded DSN key belongs to.
fn go_option_class(name: &str) -> Option<GoOption> {
    match name {
        "_journal_mode" | "_journal" => Some(GoOption::Journal),
        "_busy_timeout" | "_timeout" => Some(GoOption::BusyTimeout),
        "_synchronous" | "_sync" => Some(GoOption::Synchronous),
        "_foreign_keys" | "_fk" => Some(GoOption::ForeignKeys),
        "_cache_size" => Some(GoOption::CacheSize),
        _ => None,
    }
}

/// `consts.DefaultDbPath`'s basename and parameter string, derived from `nd-conf`
/// so the two cannot drift apart. Used only by [`is_navidrome_default_dsn`].
fn default_db_parts() -> (&'static str, &'static str) {
    let text = nd_conf::consts::DEFAULT_DB_PATH;
    let (head, query) = match text.find('?') {
        Some(at) => (&text[..at], &text[at + 1..]),
        None => (text, ""),
    };
    let basename = match head.rfind('/') {
        Some(sl) => &head[sl + 1..],
        None => head,
    };
    (basename, query)
}

fn invalid_dsn(input: &str, reason: &str) -> Error {
    Error::InvalidDsn {
        input: input.to_string(),
        reason: reason.to_string(),
    }
}

/// `strings.ToUpper(val)` against go-sqlite3's journal list
/// (v1.14.52 `sqlite3.go:1446-1457`).
fn parse_journal(input: &str, key: &str, value: &str) -> Result<JournalMode, Error> {
    let mode = match value.to_uppercase().as_str() {
        "DELETE" => JournalMode::Delete,
        "TRUNCATE" => JournalMode::Truncate,
        "PERSIST" => JournalMode::Persist,
        "MEMORY" => JournalMode::Memory,
        "WAL" => JournalMode::Wal,
        "OFF" => JournalMode::Off,
        _ => {
            return Err(invalid_dsn(
                input,
                &format!(
                    "invalid {key}: '{value}', expecting 'DELETE TRUNCATE PERSIST MEMORY WAL OFF'"
                ),
            ));
        }
    };
    Ok(mode)
}

/// Resolve `_busy_timeout` into the domain SQLite can hold, so a DSN go-sqlite3 opens
/// never fails this port's read-back ([`SQLITE_BUSY_TIMEOUT_MAX_MS`] holds the
/// measurements). In range the value is applied verbatim; outside it — non-positive or
/// above `i32::MAX` — it resolves to **0**, which is exactly what the driver's handle
/// reports for the same DSN, because go-sqlite3 sends the number as pragma text and
/// SQLite's `sqlite3Atoi` returns 0 for it. Only a value Go's own `strconv.ParseInt`
/// rejects (non-numeric, hex, `_` separators, a decimal, a `+` sign, which percent-
/// decoding turns into a space, or an `int64` overflow) is an error here, mirroring
/// Go's `Invalid _busy_timeout` on first use. No DSN is rejected, and no read-back
/// fails, because of its `busy_timeout` value. Measured with the driver: `%2B5` decodes
/// to `+5`, which Go's ParseInt accepts (a leading sign is valid) and which this port
/// resolves to 5, while a RAW `+` decodes to a space and fails on both sides.
fn parse_busy_timeout(input: &str, key: &str, value: &str) -> Result<u64, Error> {
    match value.parse::<i64>() {
        Ok(ms) if ms <= 0 => Ok(0),
        Ok(ms) if ms > i64::from(i32::MAX) => Ok(0),
        Ok(ms) => Ok(ms as u64),
        Err(_cause) => Err(invalid_dsn(
            input,
            &format!("invalid {key}: '{value}', expecting an integer millisecond count"),
        )),
    }
}

/// go-sqlite3's synchronous list (v1.14.52 `sqlite3.go:1545-1551`).
fn parse_synchronous(input: &str, key: &str, value: &str) -> Result<Synchronous, Error> {
    let mode = match value.to_uppercase().as_str() {
        "0" | "OFF" => Synchronous::Off,
        "1" | "NORMAL" => Synchronous::Normal,
        "2" | "FULL" => Synchronous::Full,
        "3" | "EXTRA" => Synchronous::Extra,
        _ => {
            return Err(invalid_dsn(
                input,
                &format!("invalid {key}: '{value}', expecting '0 OFF 1 NORMAL 2 FULL 3 EXTRA'"),
            ));
        }
    };
    Ok(mode)
}

/// go-sqlite3's boolean list for `_foreign_keys`/`_fk`
/// (v1.14.52 `sqlite3.go:1408-1416`), matched with `strings.ToLower`.
fn parse_boolean(input: &str, key: &str, value: &str) -> Result<bool, Error> {
    match value.to_lowercase().as_str() {
        "0" | "no" | "false" | "off" => Ok(false),
        "1" | "yes" | "true" | "on" => Ok(true),
        _ => Err(invalid_dsn(
            input,
            &format!(
                "invalid {key}: '{value}', expecting boolean value of '0 1 false true no yes off on'"
            ),
        )),
    }
}

/// The effective state SQLite reports for one handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveState {
    /// `PRAGMA journal_mode` (lowercase name).
    pub journal_mode: String,
    /// `PRAGMA busy_timeout` (ms).
    pub busy_timeout: i64,
    /// `PRAGMA synchronous` (0..3).
    pub synchronous: i64,
    /// `PRAGMA foreign_keys` (0/1).
    pub foreign_keys: bool,
    /// `PRAGMA cache_size` (positive pages or negative KiB).
    pub cache_size: i64,
    /// `PRAGMA database_list` file name of the main DB (`None` for a memory DB).
    pub db_file: Option<String>,
}

/// Read the five settings plus the main DB file name off a live handle. These
/// pragmas are SQLite's own truth — and `PRAGMA busy_timeout=N` returns a row, so
/// it is read with `query_row` (`Connection::execute` fails with
/// `ExecuteReturnedResults`).
pub fn effective_state(conn: &Connection) -> Result<EffectiveState, RsError> {
    Ok(EffectiveState {
        journal_mode: conn.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))?,
        busy_timeout: conn.query_row("PRAGMA busy_timeout", [], |row| row.get::<_, i64>(0))?,
        synchronous: conn.query_row("PRAGMA synchronous", [], |row| row.get::<_, i64>(0))?,
        foreign_keys: conn.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))? != 0,
        cache_size: conn.query_row("PRAGMA cache_size", [], |row| row.get::<_, i64>(0))?,
        db_file: database_file(conn)?,
    })
}

/// `PRAGMA database_list` file name of the main DB. SQLite canonicalizes it to an
/// absolute path (measured: `/private/tmp/…` for a `/tmp/…` open) and reports
/// `""` for a memory DB — never a name carrying a query string.
pub fn database_file(conn: &Connection) -> Result<Option<String>, RsError> {
    let file = conn.query_row("PRAGMA database_list", [], |row| {
        row.get::<_, Option<String>>(2)
    })?;
    Ok(match file {
        Some(name) if name.is_empty() => None,
        other => other,
    })
}

/// Apply `want` on `conn` with explicit per-connection `PRAGMA` statements and
/// verify SQLite's read-back. `path` is only the diagnostic label.
///
/// Only the settings the DSN actually named are emitted and verified.
/// `journal_mode = None` / `foreign_keys = None` mean go-sqlite3 execs no such
/// pragma (`sqlite3.go:1865-1867`, `:1851-1853`), so nothing is asserted about
/// them — a file already in WAL still reports `wal`, and this build's FK default
/// stays whatever `-DSQLITE_DEFAULT_FOREIGN_KEYS=1` gives.
///
/// The one accepted journal downgrade is a memory DB asked for WAL: SQLite answers
/// `PRAGMA journal_mode=wal` there with `memory` and no error
/// (`sqlite3.c:64434-64436`). Any other read-back difference is a hard failure — a
/// pragma SQLite ignored must not pass as configured.
///
/// # Errors
/// [`Error::Failed`] when SQLite refuses a pragma, [`Error::OpenConfiguration`]
/// when the effective state differs from the request.
pub fn apply_open_options(
    conn: &Connection,
    want: DbOpenOptions,
    is_memory: bool,
    path: &str,
) -> Result<(), Error> {
    // Order follows go-sqlite3's driver (`Open`, v1.14.52): busy timeout (`:1665`),
    // foreign keys (`:1852`), journal mode (`:1866`), synchronous (`:1906`). It is
    // kept because it is Go's order, NOT because anything here depends on it:
    // measured on this build in both orders, `PRAGMA journal_mode=wal` changes
    // neither `busy_timeout` nor `synchronous` (a fresh handle reads 5000/2 before
    // the switch and 5000/2 after it). SQLite consults `db->busyTimeout` only later,
    // when WAL enables blocking locks for a write (`walEnableBlocking`
    // `sqlite3.c:66937-66945`, `sqlite3WalWriteLock` `:66965-66977`), and it applies
    // `SQLITE_DEFAULT_WAL_SYNCHRONOUS` to a btree only when that differs from
    // `SQLITE_DEFAULT_SYNCHRONOUS` (`:73776`, `:73875`); `PRAGMA compile_options` on
    // this build reports both as 2, so no WAL branch can overwrite either value here.
    // Synchronous stays last, like Go (`:1906`), so an explicit `_synchronous` beats
    // any WAL default wherever SQLite does have one.
    //
    // `PRAGMA busy_timeout=N` returns a row: query_row, never execute.
    conn.query_row(
        &format!("PRAGMA busy_timeout={}", want.busy_timeout),
        [],
        |row| row.get::<_, i64>(0),
    )
    .map_err(|cause| Error::Failed {
        path: path.to_string(),
        cause,
    })?;
    // Foreign keys before journal mode, like Go (`:1852` before `:1866`).
    if let Some(enabled) = want.foreign_keys {
        conn.execute(
            &format!("PRAGMA foreign_keys={}", if enabled { 1 } else { 0 }),
            [],
        )
        .map_err(|cause| Error::Failed {
            path: path.to_string(),
            cause,
        })?;
    }
    // An unnamed journal mode emits no pragma at all, matching Go's
    // `if journalMode != ""` (`sqlite3.go:1865-1867`): SQLite then keeps whatever
    // the file already uses, which is the state Go's handle has.
    let journal_answer = match want.journal_mode {
        Some(mode) => conn
            .query_row(
                &format!("PRAGMA journal_mode={}", mode.sqlite_name()),
                [],
                |row| row.get::<_, String>(0),
            )
            .map(Some)
            .map_err(|cause| Error::Failed {
                path: path.to_string(),
                cause,
            })?,
        None => None,
    };
    conn.execute(
        &format!("PRAGMA synchronous={}", want.synchronous.code()),
        [],
    )
    .map_err(|cause| Error::Failed {
        path: path.to_string(),
        cause,
    })?;

    // The driver's fifth pragma runs after synchronous (`sqlite3.go:1917-1921`).
    if let Some(cache_size) = want.cache_size {
        conn.execute(&format!("PRAGMA cache_size={cache_size}"), [])
            .map_err(|cause| Error::Failed {
                path: path.to_string(),
                cause,
            })?;
    }

    let got = effective_state(conn).map_err(|cause| Error::Failed {
        path: path.to_string(),
        cause,
    })?;
    let mut wrong: Vec<String> = Vec::new();
    if let Some(cache_size) = want.cache_size {
        // Keep rendering the normalized int64 request above, just like Go's
        // ParseInt/PRAGMA pair (`sqlite3.go:1572-1577`, `:1917-1921`). Shipped
        // libsqlite3-sys 0.30.1 (`rust/Cargo.lock:667-668`) resolves that TEXT via
        // PragTyp_CACHE_SIZE/sqlite3Atoi (`sqlite3.c:139581`, `:139586`). Atoi
        // initializes x=0 and ignores GetInt32's failure (`:35917`); GetInt32
        // (`:35858`) leaves x untouched on overflow (`:35899`, `:35903`). Thus
        // out-of-int32 requests resolve to zero, NOT driver-side truncation.
        if let Some(mismatch) = cache_size_mismatch(cache_size, got.cache_size) {
            wrong.push(mismatch);
        }
    }
    // Journal mode: verified only when it was requested. Comparison is
    // case-insensitive because SQLite reports lowercase while Go's driver passes
    // the uppercase name to the pragma.
    if let Some(mode) = want.journal_mode {
        let named = mode.sqlite_name();
        let reached = got.journal_mode.to_lowercase() == named;
        let answered = journal_answer
            .as_ref()
            .is_none_or(|answer| answer.to_lowercase() == named);
        if !reached || !answered {
            // The one accepted downgrade is a memory DB that was asked for WAL:
            // SQLite keeps `memory` and reports no error
            // (`sqlite3.c:64434-64436`, `sqlite3PagerSetJournalMode` refuses any
            // journal but MEMORY/OFF for an in-memory DB), which is also where
            // Go's memory handle ends up. A DELETE/TRUNCATE/PERSIST request that
            // reads back `memory` is a real mismatch and is reported.
            let memory_downgrade = is_memory
                && mode == JournalMode::Wal
                && got.journal_mode.to_lowercase() == JournalMode::Memory.sqlite_name();
            if !memory_downgrade {
                wrong.push(format!(
                    "journal_mode requested {named} but SQLite reports {}",
                    got.journal_mode
                ));
            }
        }
    }
    // `want.busy_timeout` is already inside the i32 domain that
    // [`parse_busy_timeout`] resolves to, so the comparison is exact rather than
    // saturating — an out-of-domain request would have been resolved at parse time.
    if got.busy_timeout != want.busy_timeout as i64 {
        wrong.push(format!(
            "busy_timeout requested {} but SQLite reports {}",
            want.busy_timeout, got.busy_timeout
        ));
    }
    if got.synchronous != want.synchronous.code() {
        wrong.push(format!(
            "synchronous requested {} but SQLite reports {}",
            want.synchronous.code(),
            got.synchronous
        ));
    }
    // FK is verified only when it was requested: an unset option means "Go said
    // nothing", and this build's value then comes from
    // `-DSQLITE_DEFAULT_FOREIGN_KEYS=1`, which must not be asserted as a request.
    if let Some(enabled) = want.foreign_keys {
        if got.foreign_keys != enabled {
            wrong.push(format!(
                "foreign_keys requested {enabled} but SQLite reports {}",
                got.foreign_keys
            ));
        }
    }
    if wrong.is_empty() {
        Ok(())
    } else {
        Err(Error::OpenConfiguration {
            path: path.to_string(),
            reason: wrong.join("; "),
        })
    }
}
/// The argument SQLite is actually handed for `path` — the SQLite-argument half of
/// [`parse_open_input`]: `:memory:` becomes the shared-cache URI (`db/db.go:54-55`),
/// a file: URI stays verbatim, and a non-URI DSN loses its whole query. Only the
/// default DSN synthesizes a file: URI, with its path percent-encoded and
/// `cache=shared` forwarded. Plain filenames otherwise pass through unchanged.
///
/// An input this port rejects is returned verbatim rather than guessed at, so a
/// caller still sees SQLite's own reason; [`open`] parses the original input itself
/// and reports [`Error::InvalidDsn`].
#[must_use]
pub fn effective_path(path: &str) -> String {
    match parse_open_input(path) {
        Ok(target) => target.sqlite_arg,
        Err(_rejected) => path.to_string(),
    }
}

/// Go's `ConnectHook` equivalent (contract §4: per connection). SEEDEDRAND keeps
/// a connection-scoped `Hasher` in the closure: like Go's shared `HashFunc()`
/// instance, an unseeded id reseeds once and stays cached (so ORDER BY is
/// permutation-stable within a query). Non-deterministic across connections —
/// Go's `RegisterFunc(..., false)` (db/db.go:46).
pub fn register_extensions(conn: &Connection) -> Result<(), RsError> {
    conn.create_collation(NATSORT_COLLATION, move |a, b| {
        to_ordering(compare_fold(a, b))
    })?;
    conn.create_scalar_function("SEEDEDRAND", 2, FunctionFlags::SQLITE_UTF8, {
        let h = Hasher::new();
        move |ctx: &Context| -> Result<i64, RsError> {
            let id: String = ctx.get(0)?;
            let value: String = ctx.get(1)?;
            let fn_ = h.hash_func();
            Ok(fn_(&id, &value) as i64)
        }
    })
}

/// Go `CompareFold` returns negative/zero/positive; SQLite collations want
/// `std::cmp::Ordering`.
pub fn to_ordering(v: i32) -> Ordering {
    if v < 0 {
        Ordering::Less
    } else if v > 0 {
        Ordering::Greater
    } else {
        Ordering::Equal
    }
}

/// Applies the single fresh migration with FK pragma off around it and the
/// handle's own FK state restored afterwards (`db/db.go:85-91`). Idempotent: every
/// CREATE carries `IF NOT EXISTS`, and the statements run individually (rusqlite
/// batch splitting would corrupt trigger bodies, which contain `;`).
///
/// DB-03 point 4: the pragma is PER CONNECTION, so Go's unconditional
/// `foreign_keys=on` (`db/db.go:85-91` — unconditional because *Navidrome's default*
/// DSN asks for FK on; a user-configured `DbPath` without that parameter leaves
/// go-sqlite3's `foreignKeys` at `-1` and its handle at FK **off**, measured) is
/// replaced with a restore of the state the handle had when migration started — the
/// state [`open`] configured from [`DbOpenOptions::foreign_keys`] — so a caller that
/// deliberately requested FK off keeps it off instead of silently regaining
/// enforcement.
///
/// What is guaranteed: on all three paths (failed disable, failed DDL statement,
/// normal completion) the restore is attempted before returning and read back with
/// `PRAGMA foreign_keys`. If the restore itself fails or does not read back, this
/// function returns [`Error::OpenConfiguration`] naming the state that leaked, so an
/// `Ok(())` means the handle's FK state is the one it was configured with; an `Err`
/// may leave the handle FK-off, which is why the caller must not put such a handle
/// in a pool. `nd_db::pool::Pool::new` hands the freshly opened connection out in a
/// pool-bound `Initializer` token, not in the idle queue. Its initialization helper
/// drops the token on error; checked admission rejects changed state. A failed
/// migration cannot silently return a misconfigured lease (nd-db is downstream,
/// so the type's path is spelled, not linked).
///
/// # Errors
/// [`Error::Failed`] when SQLite refuses a DDL statement, or when the *disable*
/// pragma fails and the restore nevertheless succeeded (the handle is then in its
/// original state, so the failure is the DDL precondition, not a leak). Once the DDL
/// has run, a restore that fails or does not read back is reported as
/// [`Error::OpenConfiguration`] naming the FK state that leaked, because at that
/// point the handle is about to be handed to a caller. A disable or restore failure
/// that leaves the pragma off is also [`Error::OpenConfiguration`].
pub fn apply_fresh_schema(conn: &Connection) -> Result<(), Error> {
    apply_fresh_schema_with(conn, None)
}

/// [`apply_fresh_schema`] with a probe that runs BETWEEN the FK disable and the
/// restore, on the same connection. It exists so a test can assert the toggle from
/// the production function instead of re-expressing it: rusqlite exposes an
/// authorizer only under its `hooks` feature, which this workspace does not enable,
/// and the DDL itself never touches `PRAGMA foreign_keys`, so nothing else can
/// observe the pragma while the statements run. Read-only is a contract, not a
/// type restriction: a mutating probe can still return Ok if FK restoration succeeds.
///
/// # Panics
/// Probe calls use `catch_unwind(AssertUnwindSafe(...))`. FK is restored AND read
/// back before unwinding resumes. On successful restore the original payload is
/// propagated unchanged. Only on restore failure/mis-read is the payload replaced
/// by `(original_payload, Error::OpenConfiguration)` (the original is a
/// `Box<dyn Any + Send>`); callers downcasting their payload see a changed type in
/// that case. The error names the leaked FK state rather than being discarded.
///
/// # Errors
/// Same as [`apply_fresh_schema`].
pub fn apply_fresh_schema_with(
    conn: &Connection,
    mut between: Option<&mut dyn FnMut(&Connection)>,
) -> Result<(), Error> {
    // `want` is captured BEFORE anything is disabled, and Go's `defer`
    // (`db/db.go:85-91`) restores unconditionally — including when the disable
    // itself fails — so the restore is attempted on every path below.
    let want = foreign_keys_of(conn, "fresh_schema")?;
    if let Err(cause) = set_foreign_keys(conn, false, "fresh_schema: disable foreign_keys") {
        return match set_foreign_keys(conn, want, "fresh_schema: restore foreign_keys") {
            Ok(()) => Err(cause),
            Err(restore) => Err(Error::OpenConfiguration {
                path: "fresh_schema".to_string(),
                reason: format!(
                    "foreign_keys could not be disabled for the fresh DDL ({cause}) and could not be restored: {restore}"
                ),
            }),
        };
    }
    for sql in FRESH_STATEMENTS {
        if let Err(e) = conn.execute(sql, []) {
            // Restore first: leaving FK off after a half-applied DDL is exactly the
            // leak DB-03 point 4 forbids. Report the restore failure together with
            // the DDL cause instead of swallowing either.
            return match restore_after_ddl(conn, want, &format!("DDL statement failed ({e})")) {
                Ok(()) => Err(Error::Failed {
                    path: "fresh_schema".to_string(),
                    cause: e,
                }),
                Err(restore) => Err(Error::OpenConfiguration {
                    path: "fresh_schema".to_string(),
                    reason: restore.to_string(),
                }),
            };
        }
        // The probe runs once a statement has actually executed, so a test samples
        // the pragma in the middle of a real DDL pass.
        if let Some(probe) = between.as_deref_mut() {
            if let Err(payload) =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| probe(conn)))
            {
                match restore_after_ddl(conn, want, "the fresh DDL probe panicked") {
                    Ok(()) => std::panic::resume_unwind(payload),
                    Err(restore) => std::panic::resume_unwind(Box::new((payload, restore))),
                }
            }
        }
    }
    // Success path: the DDL has run and the handle is about to be handed to a
    // caller, so a restore that fails or mis-reads is a LEAKED STATE, not an
    // ordinary pragma failure — report it as `OpenConfiguration` naming what the
    // handle is actually left at. Measured (rusqlite `hooks` authorizer probe in a
    // scratch copy, DDL allowed, restore denied): the function used to return
    // `Failed` while the handle stayed at foreign_keys = false.
    restore_after_ddl(conn, want, "the fresh DDL completed")
}

/// Restore the handle's FK state after the DDL pass has started and report any
/// failure as [`Error::OpenConfiguration`] naming the state the handle is ACTUALLY
/// left at ([`apply_fresh_schema`]). `context` says which path got here — a completed
/// DDL or a failed statement — and both are leaks once the pragma cannot be restored.
fn restore_after_ddl(conn: &Connection, want: bool, context: &str) -> Result<(), Error> {
    match set_foreign_keys(conn, want, "fresh_schema: restore foreign_keys") {
        Ok(()) => Ok(()),
        Err(cause) => {
            // Read the handle's real state so the message says what leaked rather
            // than only what was asked for; a failing read leaves the cause alone.
            let leaked = foreign_keys_of(conn, "fresh_schema: restore foreign_keys")
                .map(|state| state.to_string())
                .unwrap_or_else(|_| "unreadable".to_string());
            Err(Error::OpenConfiguration {
                path: "fresh_schema".to_string(),
                reason: format!(
                    "{context}: foreign_keys could not be restored to {want}, handle is left at foreign_keys={leaked} ({cause})"
                ),
            })
        }
    }
}

/// `PRAGMA foreign_keys` on this handle (true = enforced).
fn foreign_keys_of(conn: &Connection, label: &str) -> Result<bool, Error> {
    conn.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
        .map(|value| value != 0)
        .map_err(|cause| Error::Failed {
            path: label.to_string(),
            cause,
        })
}

/// Set `PRAGMA foreign_keys` and verify SQLite's read-back: a pragma that was
/// ignored must not pass as applied.
///
/// # Errors
/// [`Error::Failed`] when SQLite refuses the pragma, [`Error::OpenConfiguration`]
/// when the value does not read back.
pub fn set_foreign_keys(conn: &Connection, enabled: bool, label: &str) -> Result<(), Error> {
    conn.execute(
        &format!("PRAGMA foreign_keys={}", if enabled { 1 } else { 0 }),
        [],
    )
    .map_err(|cause| Error::Failed {
        path: label.to_string(),
        cause,
    })?;
    let got = foreign_keys_of(conn, label)?;
    if got == enabled {
        Ok(())
    } else {
        Err(Error::OpenConfiguration {
            path: label.to_string(),
            reason: format!("foreign_keys requested {enabled} but SQLite reports {got}"),
        })
    }
}

/// Table names on a connection (sqlite internals excluded; FTS5 virtual tables
/// included by name) — for tests and #05 sanity checks.
pub fn schema_tables(conn: &Connection) -> Result<Vec<String>, RsError> {
    let sql =
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND substr(name,1,7) <> 'sqlite_'";
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query([])?;
    let mut names: Vec<String> = Vec::new();
    loop {
        let next = rows.next();
        let row = match next {
            Ok(Some(r)) => Some(r),
            Ok(None) => None,
            Err(e) => return Err(e),
        };
        match row {
            Some(r) => {
                let name: String = r.get(0)?;
                names.push(name);
            }
            None => break,
        }
    }
    Ok(names)
}

/// Sorted fresh table names (the golden from `schema_ddl`).
pub fn golden_sorted_table_names() -> Vec<String> {
    let names: Vec<String> = TABLE_NAMES.iter().map(|name| name.to_string()).collect();
    let mut sorted = names;
    sorted.sort();
    sorted
}

/// Resolve SQLite's signed-int32 TEXT conversion without changing the int64
/// request that is rendered into the pragma. Kept private with explicit inputs
/// so mismatch diagnostics can be tested without enabling SQLite hooks.
fn cache_size_mismatch(requested: i64, actual: i64) -> Option<String> {
    let resolved = i32::try_from(requested).map_or(0, i64::from);
    (actual != resolved).then(|| {
        format!(
            "cache_size requested {requested}, SQLite resolves to {resolved}, but reports {actual}"
        )
    })
}

#[cfg(test)]
mod cache_size_resolution_tests {
    use super::cache_size_mismatch;

    #[test]
    fn signed_overflow_mismatch_names_request_resolved_and_actual_values() {
        for requested in [2147483648, -2147483649] {
            assert_eq!(cache_size_mismatch(requested, 0), None);
            let expected =
                format!("cache_size requested {requested}, SQLite resolves to 0, but reports 17");
            assert_eq!(
                cache_size_mismatch(requested, 17).as_deref(),
                Some(expected.as_str())
            );
        }
    }
}
