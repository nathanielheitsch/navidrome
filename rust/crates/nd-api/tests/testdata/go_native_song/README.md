# Authenticated native song wire capture

This sibling corpus extends the unauthenticated first-slice oracle without changing it. `capture.sh` builds and runs the Go harness twice, offline, then promotes artifacts only when the complete captures are byte-identical. The harness dispatches through `nativeapi.New`, `server.JWTVerifier`, and the native router's real auth middleware into `httptest.ResponseRecorder`; it opens no listener and launches no production server or CLI.

## Real inputs and isolation

- One private shell-owned staging root contains the Go build cache, `go list` output, scratch working roots, and temporary captures. The shell installs cleanup traps immediately; failed captures remove their owned output reservation, and cleanup errors make the command fail. The harness scratch roots are under the staging root's private `TMPDIR`.
- The capture uses a migrated scratch SQLite datastore and real embedded migrations. Two `storagetest.FakeFS` instances and `storagetest.Template`/`Track` helpers create tag-bearing synthetic tracks; `scanner.New(...).ScanAll(..., true)` creates the media rows. No media rows are hand-inserted, and no repository mocks or committed audio bytes are used.
- Disposable admin and regular identities have distinct library grants. The regular identity successfully reads its library-1 song and is denied the exact library-2 row that the admin successfully read. This is a same-record scope control.
- `auth.Init` initializes the scratch DB signing keys, and `auth.CreateToken` mints signed test sessions accepted by the real `JWTVerifier` and `Authenticator`. The no-session request is sent without an authorization header. This proves middleware-authenticated sessions, not password login. Passwords and tokens are not captured or logged.

## Cases and observed results

Eight GET responses are recorded: authorized list and detail, authenticated missing ID, unauthenticated detail, regular-user granted-library detail, and both sides of the same-record authorized/denied control (plus an initial authorized detail precondition). Captured observations are: list 200 with count 2 and exactly the two scanner-produced IDs; authorized details and granted-library detail 200; missing and denied detail 404; no-session detail 401. Rust fixture tests pin these observations, require the exact list row set and count, and include same-length body corruption and changed-status negative controls.

## Source and toolchain provenance

Before building, the script runs `go list -deps -json` under the same local-only settings as the build. The harness derives a sorted inventory of actual local-module compiled source inputs and embedded files, hashes each file's current bytes, and records an inventory digest. It requires the scanner traversal sources, `persistence/sql_base_repository.go`, `resources/mappings.yaml`, `resources/mime_types.yaml`, and embedded migration files to appear. The Rust fixture-only test validates the recorded inventory structure and digest against a fixed pin without opening Go source/resource files or invoking Git. Fresh capture independently derives hashes from current disk bytes and requires two runs to match. The manifest also records Go version, build tags, OS/architecture, SQLite runtime, `go.mod`/`go.sum`, and capture harness inputs.

## Deterministic transformations

Scanner-generated IDs and timestamps vary across fresh scratch databases. The harness replaces only the two scanned row IDs in request paths and response bodies with stable library-specific labels and replaces JSON `createdAt`/`updatedAt`/`birthTime` strings with a fixed UTC value. Synthetic file size is fixed in the test tags. Status, response headers, content type, remaining body bytes, and JSON formatting are retained. Credential response headers are omitted defensively; request credentials are never serialized.

## Reproduce

From any working directory, pass a destination that does not already exist:

```sh
sh /path/to/repository/rust/crates/nd-api/tests/testdata/go_native_song/capture.sh /absolute/new/output
```

Relative output paths are anchored to the invocation directory before the script changes directory. The script atomically reserves the destination with `mkdir`, refuses files/directories/symlinks and races, and never merges or overwrites. It disables Go environment/workspace files, proxies, checksum services, private-module proxy bypasses, VCS fetching, toolchain downloads, and module writes; configuration values are checked locally without a network probe.
