# go_native — API-00 in-process Go native oracle harness

Test-only harness that captures the **real** Go native API wire responses for the API-00
corpus (`docs/rust-port/subplans/boot-api.md:171-183`). Tracked corpus lives in
`rust/crates/nd-api/tests/data/native/` (`manifest.json`, `cases/*.json`).

## Origin (stated explicitly)

**IN-PROCESS GO NATIVE ROUTER CAPTURE.** Requests are dispatched through the exported
`server/nativeapi.New` router and its real middleware chain into
`net/http/httptest` recorder-style sinks.

- **NOT** a production-binary end-to-end run: no `navidrome` binary is built or launched,
  no listener is opened, no socket is bound, no network traffic occurs.
- **NOT** a mock capture: no `tests.MockDataStore` and no mock repositories. The datastore is
  `github.com/navidrome/navidrome/persistence` over a real SQLite file migrated by the real
  embedded goose migrations through `db.Init`, so the body bytes come from the production
  serializer.
- **NOT** a Rust-side or hand-written fixture: every byte in `cases/*.json` is what the Go
  chain actually wrote. Tolerances live in `requests.json` (the spec) and in each case's
  `verdict` field; they never rewrite the observed bytes.

## What is real

| Piece | Source |
|---|---|
| Router + routes | `server/nativeapi/native_api.go:59-104` (`routes()`), with `rx()` at `:106-121` → `rest.GetAll`/`rest.Get` |
| Protected group middlewares | `server.Authenticator`, `server.JWTRefresher`, `server.UpdateLastAccessMiddleware` (`native_api.go:67-69`; `Authenticator` is `server/auth.go:272-283`) |
| Outer JWT verification layer | `server.JWTVerifier` (`server/auth.go:186-188`), applied exactly as `server.Server.initRoutes` does (`server/server.go:170-185`, `JWTVerifier` at `:183`) |
| Mounting | mirrors `Server.MountRouter`: `r.Group(func(r){ r.Mount(path.Join(BasePath, consts.URLPathNativeAPI), router) })` (`server/server.go:50-56`, `cmd/root.go:128`) |
| JSON envelope | `github.com/deluan/rest@v1.0.1` `render.go:9-24` (`RespondWithError`/`RespondWithJSON`) and `controller.go:26-63` (`Get`/`GetAll`) |
| 404 for unregistered path | chi `mix` not-found responder → `http.NotFound` plain text |
| DataStore | `persistence.New(db.Db())`, real SQLite + goose migrations |
| Config | `conf.InitConfig` + `conf.Load` against a synthetic TOML in a private temp root |

## Layering fidelity (what is *not* reproduced)

`server.Server.initRoutes` applies this middleware stack before the mounted native router:
`secureMiddleware`, `corsHandler`, `middleware.RequestID`, `realIPMiddleware`,
`middleware.Recoverer`, `middleware.Heartbeat("/ping")`, `robotsTXT`, `serverAddressMiddleware`,
`clientUniqueIDMiddleware`, `compressMiddleware`, `loggerInjector`, `JWTVerifier`.

This harness reproduces **only `JWTVerifier` plus the mount**, because that is the layer the
captured contract needs and every other middleware either requires network/identity
infrastructure (`realIPMiddleware`, `serverAddressMiddleware`, `clientUniqueIDMiddleware`) or
only changes encoding (`compressMiddleware`). Consequences, recorded rather than hidden:

- Security headers from `secureMiddleware` (`x-frame-options`, `referrer-policy`,
  `permissions-policy`, and its `content-type-options: nosniff`) are **absent** from the
  captured headers. The `x-content-type-options: nosniff` seen on the plain-text 404 comes
  from Go's own not-found responder, not from `secureMiddleware`.
- No `Access-Control-*` headers, no `X-Request-Id`, no `Vary`/`Content-Encoding` from
  compression.
- A later full-corpus capture that wants production response headers must reproduce the whole
  stack; until then these fixtures pin the **route/auth/serialization** contract only.

Response headers are snapshotted at the first write (`writeOnceRecorder`), because
`UpdateLastAccessMiddleware` mutates `Header()` *after* the handler writes on the first call
per user — on a real socket those late additions cannot reach a flushed response.

## Injected dependencies (exact)

```go
nativeapi.New(
    ds, nil,                       // share: route not registered (EnableSharing = false)
    playlists.NewPlaylists(ds, nil), // imgUpload only used by upload handlers
    nil,                           // insights: only dereferenced inside the /insights handler
    core.NewLibrary(ds, nil, nil, nil, nil), // scanner/watcher/broker/pluginManager inert
    core.NewUser(ds, nil),         // pluginManager only used by the delete path
    nil, nil, nil, nil, nil,       // maintenance, pluginManager, uploader, external, quickConnect
)
```

`api.users` and `api.libs` **must** be non-nil: route registration dereferences them at
construction (`native_api.go:70`, `:99`), which is exactly why the existing Go song test injects
mock services (`native_api_song_test.go:102`). Here they are the **real** production constructors
(`core.NewUser`, `core.NewLibrary`) with inert nil dependencies, so nothing is substituted and
nothing of theirs is exercised by this slice.

`cmd.CreateNativeAPIRouter` / `cmd/wire_gen.go:66ff` is deliberately **not** used: it also builds
the scanner, watcher, plugin manager, external provider and metrics, whose constructors start
background goroutines this capture does not need (parent isolation-audit decision).

## Isolation

- Private temp root (`os.MkdirTemp`, 0700) with separate `home`, `config`, `music`, `data`,
  `cache` sub-roots; cleaned up per the lifecycle rules below. Repository files are read
  read-only (`go.mod`/`go.sum` for pins, tracked build inputs for the on-disk digest) and
  never written; no user config or credential is read.
- `os.Clearenv()` then a minimal allowlist (`PATH`, `TMPDIR`, `USER`, `LOGNAME`, `SHELL`,
  `TERM`, `LANG`, `LC_ALL`, `GOROOT`, `GOPATH`, `GOCACHE`, `GOMODCACHE`, SSL cert vars); all
  inherited `ND_*` variables are unset; `HOME` is the temp home; cwd is moved into the temp
  root so relative defaults cannot reach the repository.
- Synthetic TOML only: no real config, no credentials. `Address`/`Port` are set solely to
  satisfy config validation — **no listener is ever opened**.
- External/background integrations disabled at the config layer:
  `EnableExternalServices=false`, `Scanner.Enabled=false`, `Scanner.ScanOnStartup=false`,
  `Plugins.Enabled=false`, `Prometheus.Enabled=false`, `EnableSharing=false`,
  `EnableInsightsCollector=false`, `DevActivityPanel=false`, empty
  `DevAutoCreateAdminPassword`/`DevAutoLoginUsername`.
- `auth.Init(ds)` mirrors production startup (`server/server.go:41`); it loads/creates JWT
  secrets **inside the isolated DB**. This slice mints no token and performs no login.
- Build/run with `-mod=readonly`; no dependency or `go.mod`/`go.sum` changes.

## Redaction

`redact()` replaces credential-shaped JSON fields (`token`, `password`, `secret`, `api_key`,
`access_token`, `refresh_token`, `jwt_secret`, `subsonicSalt`, `subsonicToken`) with
`"<redacted>"`; `authorization`, `x-nd-authorization`, `cookie`, `set-cookie`, `x-auth-token`
response headers are replaced with `"<redacted>"`. Status, content type, and the remaining body
bytes are preserved unchanged. The only bearer value ever sent is an unsigned, credential-free
shaped string (`unsignedBearer`). `fail()` never echoes config values, real paths, or
credentials.

## Lifecycle / safety rules (API-00 review)

- **No destructive operation on caller paths.** Neither `main.go` nor `capture.sh` ever
  `rm -rf` a caller-provided directory.
- **Existing destination is refused, not overwritten.** `main.go` `os.Lstat`s `--out` before
  any write and exits **4** if the path exists as a directory, a regular file, or a symlink
  (dangling included) — nothing is modified in that case. `capture.sh` additionally refuses
  when a supplied `OUTPUT_DIR` already contains `run1`, `run2` or `captured`.
- **Owned private staging only.** The harness creates one `os.MkdirTemp` root (0700) that it
  owns; writes there use `os.Mkdir` and `O_CREATE|O_EXCL` (never `RemoveAll`, never truncate).
  Promotion copies into the fresh destination with exclusive writes and refuses symlinks.
- **Cleanup scope (not an absolute guarantee).** `run()` returns an error to `main`; the owned
  root is removed after normal return, after an error, on panic (`recover`), and on SIGINT.
  There is no `os.Exit` inside `run()`, so `fail()` cannot bypass cleanup. A hard kill
  (`SIGKILL`), a Go runtime abort/fatal, or OOM cannot run any cleanup, so an owned root
  **can survive** those. `capture.sh` removes only its own `mktemp` staging through an `EXIT`
  trap, which likewise cannot run on a hard kill. No pre-existing or older temp directory is
  ever cleaned up by this harness.
- **Case IDs validated before any write**: unique, safe basenames
  (`^[a-z0-9][a-z0-9._-]{0,63}$`, no separators, no `.`/`..`); violation → exit **4**.
- **Verdict mismatch fails before promotion**: exit **3**, no destination written.
- **Offline toolchain, forced unconditionally.** `capture.sh` enters the repository root
  *before* building, then exports `GOTOOLCHAIN=local`, `GOPROXY=off`, `GOFLAGS=-mod=readonly`
  plainly (not `${VAR:-…}`), so an inherited online `GOPROXY`/`GOTOOLCHAIN` cannot be honored.
  The harness opens no listener and makes no request; `capture.sh` builds once and runs the
  binary directly because `go run` collapses every non-zero exit to 1.
- **Source pin is validated, not copied, and pins the *original Go ref*.** The spec field is
  `source_ref` — `b95e12ed`, the last commit touching the captured Go build inputs — **not**
  the harness or branch commit, so harness commits and Rust/docs-only commits cannot move it.
  The compared build-input set is deliberately conservative: `go.mod`, `go.sum`, and the whole
  tracked trees of `server core persistence conf consts model utils db adapters resources ui
  log scheduler plugins` (embedded inputs and migrations included), not just the import-
  reachable subset. Validation fails closed (**exit 5**) unless:
  1. `source_ref` resolves to a commit in this repository (missing or empty → exit 5);
  2. `git diff -z --name-only <ref>` over those paths is empty, so no tracked input differs
     from the ref on disk (a deleted or replaced file counts);
  3. no untracked **or ignored** file exists inside those paths (`ls-files --others`, and
     `--others --ignored --exclude-standard`) — a stray source or embedded input changes the
     build and is rejected;
  4. a digest over `path + mode + git blob hash` taken from the ref's tree equals the same
     digest computed from the **actual on-disk bytes** (symlink targets included). The digest
     reads real files, so `git update-index --assume-unchanged` cannot hide a tamper.
  Only the verified stable pin (`source_ref`, `source_ref_tree`, `go_source_sha256`) is
  serialized into the compared artifact. The current `HEAD`/tree and the offline-toolchain
  state are **informational**: printed to stdout as `INFO_WORKTREE` and deliberately kept
  outside `manifest.json`, so HEAD drift on unrelated commits cannot change `FULL_SHA256`.

Exit codes: `0` ok · `1` runtime · `2` usage · `3` verdict mismatch / non-vacuity ·
`4` refused destination or unsafe spec · `5` source-pin mismatch.

## Reproduce

```sh
sh rust/crates/nd-api/tests/testdata/go_native/capture.sh /tmp/api00   # fresh dir only
```

or directly, with an explicitly offline toolchain:

```sh
GOTOOLCHAIN=local GOPROXY=off \
go run -mod=readonly -tags 'netgo sqlite_fts5' ./rust/crates/nd-api/tests/testdata/go_native \
    --cases rust/crates/nd-api/tests/testdata/go_native/requests.json \
    --repo-root . --out /tmp/api00-run1   # /tmp/api00-run1 must not exist
```

Two runs using **different owned temp roots** produce a byte-identical **full** artifact
(manifest + all cases), and the tracked corpus under
`rust/crates/nd-api/tests/data/native/` matches a fresh run byte-for-byte:

```
CASES_SHA256 84e8d8d9ad2f11ccef7e289910ec54e58149d688631ed10873c0eef6cbd253e2
FULL_SHA256  2e95056e96366c7bbd825f2d95a3961827e0f81f4fc343374e2375b4d79a1713
```

`FULL_SHA256` hashes manifest + cases recursively; `CASES_SHA256` hashes the case bytes only
(sorted names, `name \0 bytes \0`). The synthetic-config pin is normalized
(`<HOME>`/`<CONFIG>`/`<MUSIC>`/`<DATA>`/`<CACHE>`) so it is stable across temp roots, and
because the manifest carries only the verified stable pin, the same `FULL_SHA256` reproduces
from different HEADs while the compared build inputs are unchanged.

## Non-vacuity and negative controls (all executed)

| control | result |
|---|---|
| empty spec | exit 3, `non-vacuity: cases spec contains zero requests` |
| deliberately wrong `expect_status` (401 → spec 200) | exit 3 (`go run` reports 1), `verdict mismatch on 1/4 case(s) before promotion`, **no destination written** |
| spec pins an unresolvable `source_ref` | exit 5, `pinned Go source ref … does not resolve to a commit`, nothing written |
| spec has no `source_ref` | exit 5, `spec has no source_ref` |
| tracked build input modified on disk vs the pinned ref | exit 5, `on-disk Go build inputs differ … first: conf/configuration.go` |
| tracked build input deleted on disk | exit 5 (same diff check) |
| untracked `.go` file added inside compared paths | exit 5, `untracked build input … (rejected)` |
| **ignored** file inside compared paths | exit 5, same rejection (ignored files still reach the build) |
| `git update-index --assume-unchanged` tamper (git diff reports nothing) | exit 5 — the on-disk blob digest catches it |
| pin a second commit whose tree matches disk | exit 0 (the pin follows verified inputs, not HEAD) |
| disk differs from an earlier clean ref | exit 5 |
| **HEAD drift** | identical `FULL_SHA256 2e95056e…` from two worktrees at different HEADs (`ad343831` vs `7ca5322d`), because only the stable pin is serialized |
| `case_id` `../../etc/passwd` | exit 4, refused before any write |
| duplicate `case_id` | exit 4 |
| `auth: "login"` case | exit 4 (slice scope) — the harness refuses to fake auth |
| existing dir / symlink / dangling symlink / regular file as `--out` | exit 4, sentinel content intact |
| `capture.sh --out DIR` (mistyped flag) or more than one argument | exit 2 before any write; no `./--out` directory appears in the working tree |
| inherited online `GOPROXY`/`GOTOOLCHAIN`/`GOFLAGS` | ignored — `capture.sh` exports offline values unconditionally, fresh capture still reproduces the corpus |
| two runs with different `TMPDIR` (different owned roots) | byte-identical artifact; owned-root leak count equals baseline (0) |
| owned temp root after each failure path | removed; leak count equals baseline |

Controls ran against a throwaway scratch repository (`git archive` of the pinned ref into
`/tmp` with its own `.git`) plus scratch specs and destinations in `/tmp`; the real
repository, the tracked spec and the tracked corpus were not modified by any of them.

## First slice — observed results

Behavior class: **auth middleware + unknown route only** — *not* authenticated song behavior.
No login, no token minting, no media rows, no repository/object bodies.

| case | status | content-type | body |
|---|---|---|---|
| `song_get_all_unauthenticated` | 401 | `application/json` | `{"error":"Not authenticated"}` |
| `song_get_one_unauthenticated` | 401 | `application/json` | `{"error":"Not authenticated"}` |
| `invalid_auth_token` | 401 | `application/json` | `{"error":"Not authenticated"}` |
| `unknown_route_unauthenticated` | 404 | `text/plain; charset=utf-8` | `404 page not found\n` |

`CASES_SHA256` for the slice: `84e8d8d9ad2f11ccef7e289910ec54e58149d688631ed10873c0eef6cbd253e2`.

## Deliberate deltas (for parent approval, per boot-api.md:181-183)

1. An unregistered native path returns chi/`http.NotFound` **plain text**, not the `rest`
   JSON `{"error"}` envelope. Captured as-is; do not fold into a coherent JSON error policy
   without approval.
2. The protected 401 body is `{"error":"Not authenticated"}` from `rest.RespondWithError`,
   distinct from Subsonic-style error shapes elsewhere.
3. Response headers omit the production security/CORS/compression layer (see "Layering
   fidelity"). A consumer must not treat these fixtures as a header-complete contract.

## Not in this slice

Authenticated success bodies (`GET /song`, `GET /song/{id}` with data), the other repository
routes (`/album`, `/artist`, `/genre`, `/tag`, `/scrobble`, `/translation`, `/player`, `/share`,
`/playlist`, …), persistable 405/PUT/DELETE variants, trailing-slash and BasePath variants,
login/setup, method-matrix coverage, malformed query/body classes, `/api/v1`, and stream
routes. Adding them requires new spec entries — and for media data, an authorized seed recipe,
because `media_file` rows are produced by the scanner, not by hand.
