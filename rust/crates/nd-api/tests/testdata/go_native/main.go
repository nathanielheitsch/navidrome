//go:build netgo && sqlite_fts5

// Command go_native is a TEST-ONLY, in-process oracle harness for the API-00 native wire
// corpus (docs/rust-port/subplans/boot-api.md:171-183).
//
// ORIGIN — stated explicitly, per the parent isolation-audit decision:
//
//	IN-PROCESS GO NATIVE ROUTER CAPTURE, via net/http/httptest recorder.
//	This is NOT a production-binary end-to-end capture: no navidrome binary is built or
//	launched, no listener is opened, and no network traffic occurs.
//
// BEHAVIOR CLASS — deliberately narrow. This slice covers ONLY:
//
//	(a) the native API auth middleware (unauthenticated and invalid-token refusals on
//	    protected /song routes), and
//	(b) an unregistered native route (chi/plain-text not-found).
//
// It does NOT capture authenticated song behavior: no login, no token minting, no media
// rows, no repository/object bodies. See README.md "Not in this slice".
//
// What is real (nothing fabricated or substituted):
//
//   - server/nativeapi.New — the exported router constructor; it embeds http.Handler, so
//     the handler under test is the same router production mounts at consts.URLPathNativeAPI.
//     The captured path runs the actual rx()-registered /song routes (rest.GetAll/rest.Get
//     from github.com/deluan/rest@v1.0.1) and the actual protected-group middlewares
//     server.Authenticator, server.JWTRefresher and server.UpdateLastAccessMiddleware
//     (server/nativeapi/native_api.go:66-69, route registration from :70).
//   - server.JWTVerifier, mirroring only the real production outer layer: server.Server
//     applies defaultMiddlewares (server/server.go:170-185, JWTVerifier at :183) before the
//     mounted native router. No other default middleware is reproduced; see
//     "Layering fidelity" in README.md.
//   - github.com/navidrome/navidrome/persistence over a real SQLite file in a private temp
//     directory, migrated by the real embedded goose migrations through db.Init. No mock
//     repositories and no fake datastore: server/nativeapi/native_api_song_test.go drives
//     tests.MockDataStore, whereas this harness drives the production persistence layer so
//     the observed bytes are the production serializer's output.
//   - conf.InitConfig/conf.Load against a synthetic throwaway TOML.
//
// Injected dependencies, documented precisely. Following the existing Go song test
// (server/nativeapi/native_api_song_test.go:102), collaborators the protected /song path
// never touches are passed as nil:
//
//	nativeapi.New(ds, nil /*share*/, playlists.NewPlaylists(ds, nil /*imgUpload*/),
//	    nil /*insights*/, core.NewLibrary(ds, nil,nil,nil,nil,nil),
//	    core.NewUser(ds, nil), nil /*maintenance*/, nil /*pluginManager*/,
//	    nil /*imgUpload*/, nil /*external*/, nil /*quickConnect*/)
//
// api.users and api.libs must be non-nil because route registration dereferences them at
// construction (native_api.go:70, :99); they are the REAL production constructors with
// inert nil dependencies, not substitutes. The production injector (cmd/wire_gen.go:66ff)
// also builds scanner, watcher, plugin manager, external provider and metrics — constructor
// goroutines this capture does not need — so cmd.CreateNativeAPIRouter is NOT used.
//
// Safety and lifecycle contract enforced by this file:
//
//   - Owned private staging only: everything the harness creates lives under one
//     os.MkdirTemp root (0700) that it owns and removes after normal return, after an
//     error return from run(), on panic (recover), and on SIGINT. A hard kill (SIGKILL),
//     runtime abort/fatal, or OOM cannot run cleanup, so an owned root may survive those;
//     that is a documented limitation, not a guaranteed invariant.
//   - The caller's --out is never modified or removed. If it already exists (as a
//     directory, a file, OR a symlink, including a dangling one) the harness refuses with
//     exit 4 before performing any write, so pre-existing content survives.
//   - All writes are exclusive: O_CREATE|O_EXCL for files, os.Mkdir (never RemoveAll) for
//     directories inside the owned root. Promotion copies into a fresh --out with O_EXCL and
//     aborts rather than clobbering.
//   - Case IDs are validated as unique, safe basenames BEFORE any write.
//   - A verdict mismatch fails with exit 3 BEFORE promotion.
//   - No network: GOPROXY/GOTOOLCHAIN must be offline at build time; no listener is opened;
//     requests go only to the recorder.
//   - No dependency changes: -mod=readonly; go.mod/go.sum are read only for provenance.
//   - Source pin is validated, never copied: it names the pinned ORIGINAL Go ref, and the
//     actual ON-DISK build inputs must match that ref (a git diff against the ref over the
//     compared paths must be empty) with no untracked build inputs inside them. Differences
//     fail closed. The compared artifact records ONLY the stable verified pin and digest; the
//     current HEAD/tree and the offline-toolchain state are informational, printed to stdout
//     and deliberately kept OUTSIDE the compared manifest.
//
// Credentials: this slice is entirely unauthenticated. It performs no login and mints no
// token. The only bearer value sent is an unsigned, credential-free shaped string, and
// credential-shaped fields plus Authorization/Cookie/Set-Cookie headers are redacted.
//
// Usage (repository root; requires the project's mandatory build tags, offline toolchain):
//
//	GOTOOLCHAIN=local GOPROXY=off GOFLAGS=-mod=readonly \
//	go run ./rust/crates/nd-api/tests/testdata/go_native \
//	    --cases rust/crates/nd-api/tests/testdata/go_native/requests.json \
//	    --repo-root . --out /tmp/api00-run1
//
// Exit codes: 0 success, 1 runtime failure, 2 usage, 3 verdict mismatch, 4 refused/unsafe
// destination or unsafe spec, 5 source-pin mismatch.
package main

import (
	"context"
	"crypto/sha1"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"os/signal"
	"path"
	"path/filepath"
	"regexp"
	"runtime"
	"sort"
	"strings"
	"sync"

	"github.com/go-chi/chi/v5"
	"github.com/navidrome/navidrome/conf"
	"github.com/navidrome/navidrome/consts"
	"github.com/navidrome/navidrome/core"
	"github.com/navidrome/navidrome/core/auth"
	"github.com/navidrome/navidrome/core/playlists"
	"github.com/navidrome/navidrome/db"
	"github.com/navidrome/navidrome/log"
	"github.com/navidrome/navidrome/persistence"
	"github.com/navidrome/navidrome/server"
	"github.com/navidrome/navidrome/server/nativeapi"
)

const (
	origin     = "in-process Go native router capture (net/http/httptest recorder; no listener, no network, no production binary)"
	behavior   = "auth middleware + unknown route only (NOT authenticated song behavior)"
	buildTags  = "netgo sqlite_fts5"
	authHeader = "X-ND-Authorization" // consts.UIAuthorizationHeader

	// unsignedBearer is a syntactically shaped bearer value with no valid signature and no
	// credential material; it exercises the real verification-failure path.
	unsignedBearer = "Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9." +
		"eyJzdWIiOiJub3QtYS1yZWFsLXVzZXIifQ." +
		"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"

	tempPrefix  = "api00-native-oracle."
	stagingName = "staging"
	casesName   = "cases"
)

// exit codes
const (
	exitOK      = 0
	exitRuntime = 1
	exitUsage   = 2
	exitVerdict = 3
	exitRefuse  = 4
	exitSrcPin  = 5
)

type caseSpec struct {
	CaseID            string            `json:"case_id"`
	Method            string            `json:"method"`
	Path              string            `json:"path"`
	Auth              string            `json:"auth"`
	Body              *string           `json:"body"`
	Headers           map[string]string `json:"headers"`
	ExpectStatus      int               `json:"expect_status"`
	ExpectContentType string            `json:"expect_content_type"`
	ExpectHeaders     map[string]string `json:"expect_headers"`
	BehaviorClass     string            `json:"behavior_class"`
	Provenance        json.RawMessage   `json:"provenance"`
}

type specFile struct {
	CorpusScope string     `json:"corpus_scope"`
	RoutePrefix string     `json:"route_prefix"`
	Oracle      string     `json:"oracle"`
	SourceRef   string     `json:"source_ref"`
	Behavior    string     `json:"behavior"`
	Requests    []caseSpec `json:"requests"`
}

// observed preserves status, content type and body exactly as the real chain wrote them.
type observed struct {
	CaseID        string            `json:"case_id"`
	Origin        string            `json:"origin"`
	BehaviorClass string            `json:"behavior_class"`
	RequestMethod string            `json:"request_method"`
	RequestPath   string            `json:"request_path"`
	AuthMode      string            `json:"auth_mode"`
	Status        int               `json:"status"`
	Headers       map[string]string `json:"response_headers"`
	BodyKind      string            `json:"body_kind"`
	BodyLength    int               `json:"body_length"`
	BodySHA256    string            `json:"body_sha256"`
	Body          string            `json:"body"`
	Provenance    json.RawMessage   `json:"provenance"`
	Verdict       string            `json:"verdict"`
}

type provenance struct {
	SourceRef             string   `json:"source_ref"`
	SourceRefTree         string   `json:"source_ref_tree"`
	GoSourceSHA256        string   `json:"go_source_sha256"`
	GoVersion             string   `json:"go_version"`
	BuildTags             string   `json:"build_tags"`
	SQLiteRuntime         string   `json:"sqlite_runtime"`
	OS                    string   `json:"os"`
	Arch                  string   `json:"arch"`
	Toolchain             string   `json:"toolchain"`
	ModulePins            []string `json:"module_pins"`
	GoModSHA256           string   `json:"go_mod_sha256"`
	GoSumSHA256           string   `json:"go_sum_sha256"`
	SyntheticConfigSHA256 string   `json:"synthetic_config_sha256"`
}

type manifest struct {
	Kind                string     `json:"kind"`
	Origin              string     `json:"origin"`
	Behavior            string     `json:"behavior"`
	ProductionBinaryRun bool       `json:"production_binary_run"`
	ListenerOpened      bool       `json:"listener_opened"`
	Authenticated       bool       `json:"authenticated"`
	NetworkUsed         bool       `json:"network_used"`
	CorpusScope         string     `json:"corpus_scope"`
	Provenance          provenance `json:"provenance"`
	Cases               []string   `json:"cases"`
}

// owned is the single temp root the harness created and must remove on every exit path.
type owned struct {
	root string
	once sync.Once
}

func (o *owned) cleanup() {
	if o == nil || o.root == "" {
		return
	}
	o.once.Do(func() {
		if err := os.RemoveAll(o.root); err != nil {
			// Only our own root is reported; never a caller path.
			fmt.Fprintf(os.Stderr, "go_native: cleanup of owned temp root failed: %v\n", err)
		}
	})
}

func main() {
	casesFile := flag.String("cases", "", "requests.json spec path (required)")
	repoRoot := flag.String("repo-root", ".", "repository root, read only for provenance and source-pin validation")
	outDir := flag.String("out", "", "destination directory; must NOT already exist (required)")
	flag.Parse()

	// Signal-triggered exit still runs the owned-root cleanup registered here.
	cleanup := make(chan func(), 1)
	sigCtx := make(chan os.Signal, 1)
	signal.Notify(sigCtx, os.Interrupt)

	var o *owned
	setOwned := func(root string) { o = &owned{root: root}; cleanup <- o.cleanup }

	go func() {
		select {
		case <-sigCtx:
			if f := drain(cleanup); f != nil {
				f()
			}
			os.Exit(exitRuntime)
		}
	}()

	code := func() (code int) {
		defer func() {
			if r := recover(); r != nil {
				if o != nil {
					o.cleanup()
				}
				fmt.Fprintf(os.Stderr, "go_native: panic: %v\n", r)
				code = exitRuntime
			}
		}()
		c, err := run(*casesFile, *repoRoot, *outDir, setOwned)
		if err != nil {
			fmt.Fprintf(os.Stderr, "go_native: %v\n", err)
			return c
		}
		return exitOK
	}()

	if f := drain(cleanup); f != nil {
		f()
	}
	os.Exit(code)
}

func drain(c chan func()) func() {
	select {
	case f := <-c:
		return f
	default:
		return nil
	}
}

func run(casesFile, repoRoot, outDir string, setOwned func(string)) (int, error) {
	if casesFile == "" || outDir == "" {
		return exitUsage, errors.New("--cases and --out are required")
	}

	// Resolve CLI paths before the cwd is moved into the owned root.
	for _, p := range []*string{&casesFile, &repoRoot, &outDir} {
		abs, err := filepath.Abs(*p)
		if err != nil {
			return exitRuntime, fmt.Errorf("resolve path: %w", err)
		}
		*p = abs
	}

	// Destination refusal FIRST, before any write or temp creation: an existing path of any
	// type, including a symlink (dangling or not), is refused so caller content survives.
	switch _, err := os.Lstat(outDir); {
	case err == nil:
		return exitRefuse, fmt.Errorf("refusing to write: destination already exists: %s (nothing was modified)", outDir)
	case !errors.Is(err, os.ErrNotExist):
		return exitRefuse, fmt.Errorf("refusing to write: cannot stat destination: %w", err)
	}

	log.SetLevel(log.LevelError)
	log.SetOutput(io.Discard)

	// Provenance from the repository, read-only, BEFORE isolation. The current HEAD/tree and
	// the offline-toolchain state are informational (stdout only) and are deliberately NOT
	// serialized into the compared artifact, so the corpus does not move when HEAD moves.
	// The offline label is read here, before os.Clearenv below, so it reports the toolchain
	// the harness was actually built and launched with rather than the isolated runtime env.
	offlineInfo := offlineLabel()
	worktreeLabel, worktreeTreeLabel := worktreeInfo(repoRoot)
	prov := provenance{
		GoVersion: runtime.Version(),
		BuildTags: buildTags,
		OS:        runtime.GOOS,
		Arch:      runtime.GOARCH,
		Toolchain: fmt.Sprintf("%s/%s %s", runtime.GOOS, runtime.GOARCH, runtime.Version()),
	}
	goMod, err := os.ReadFile(filepath.Join(repoRoot, "go.mod"))
	if err != nil {
		return exitRuntime, fmt.Errorf("read go.mod for provenance: %w", err)
	}
	goSum, err := os.ReadFile(filepath.Join(repoRoot, "go.sum"))
	if err != nil {
		return exitRuntime, fmt.Errorf("read go.sum for provenance: %w", err)
	}
	prov.GoModSHA256 = hexSHA256(goMod)
	prov.GoSumSHA256 = hexSHA256(goSum)
	prov.ModulePins = modulePins(string(goMod), string(goSum))

	// The owned root: the only place this harness writes besides the fresh destination.
	root, err := os.MkdirTemp("", tempPrefix)
	if err != nil {
		return exitRuntime, fmt.Errorf("create owned temp root: %w", err)
	}
	setOwned(root)
	if err := os.Chmod(root, 0700); err != nil {
		return exitRuntime, fmt.Errorf("chmod owned temp root: %w", err)
	}

	roots := map[string]string{}
	for _, name := range []string{"home", "config", "music", "data", "cache"} {
		dir, err := os.MkdirTemp(root, name)
		if err != nil {
			return exitRuntime, fmt.Errorf("create isolated %s root: %w", name, err)
		}
		if err := os.Chmod(dir, 0700); err != nil {
			return exitRuntime, fmt.Errorf("chmod %s root: %w", name, err)
		}
		roots[name] = dir
	}

	cfgBody := syntheticConfig(roots)
	cfgPath := filepath.Join(roots["config"], "navidrome.toml")
	if err := writeExclusive(cfgPath, []byte(cfgBody), 0600); err != nil {
		return exitRuntime, fmt.Errorf("write synthetic config: %w", err)
	}
	prov.SyntheticConfigSHA256 = hexSHA256([]byte(normalizePaths(cfgBody)))

	// Isolate the environment: drop everything inherited, keep a minimal allowlist, and
	// move cwd into the owned root so relative defaults cannot reach the repository.
	keep := allowedEnv()
	os.Clearenv()
	for k, v := range keep {
		_ = os.Setenv(k, v)
	}
	_ = os.Setenv("HOME", roots["home"])
	for _, kv := range os.Environ() {
		k, _, _ := strings.Cut(kv, "=")
		if strings.HasPrefix(k, "ND_") {
			_ = os.Unsetenv(k)
		}
	}
	if err := os.Chdir(roots["home"]); err != nil {
		return exitRuntime, fmt.Errorf("enter isolated cwd: %w", err)
	}

	spec, err := readSpec(casesFile)
	if err != nil {
		return exitRuntime, err
	}
	if len(spec.Requests) == 0 {
		return exitVerdict, errors.New("non-vacuity: cases spec contains zero requests")
	}
	if err := validateCaseIDs(spec.Requests); err != nil {
		return exitRefuse, err
	}
	for _, c := range spec.Requests {
		if c.Auth == "login" {
			return exitRefuse, fmt.Errorf("slice scope: case %s requests authenticated login; this slice performs no login and mints no token", c.CaseID)
		}
	}

	// The pin names the ORIGINAL Go ref; the actual on-disk build inputs must match it. This
	// is never taken from the spec alone: the ref must exist, the working tree must be clean
	// over the compared paths relative to that ref, and untracked build inputs are rejected.
	ref, err := resolveSourceRef(repoRoot, spec.SourceRef)
	if err != nil {
		return exitSrcPin, err
	}
	if err := verifyBuildInputs(repoRoot, ref); err != nil {
		return exitSrcPin, err
	}
	refTree, err := git(repoRoot, "rev-parse", ref+"^{tree}")
	if err != nil {
		return exitSrcPin, fmt.Errorf("read tree of pinned ref %s: %w", ref, err)
	}
	refDigest, err := buildInputDigest(repoRoot, ref, true)
	if err != nil {
		return exitRuntime, err
	}
	diskDigest, err := buildInputDigest(repoRoot, ref, false)
	if err != nil {
		return exitRuntime, err
	}
	if diskDigest != refDigest {
		return exitSrcPin, fmt.Errorf("on-disk Go build inputs (%s) differ from pinned ref %s (%s)", diskDigest, ref, refDigest)
	}
	prov.SourceRef = ref
	prov.SourceRefTree = refTree
	prov.GoSourceSHA256 = refDigest

	ctx := context.Background()

	conf.InitConfig(cfgPath, false)
	conf.Load(true)
	conf.Server.EnableExternalServices = false
	conf.Server.Scanner.Enabled = false
	conf.Server.Scanner.ScanOnStartup = false
	conf.Server.Plugins.Enabled = false
	conf.Server.Prometheus.Enabled = false
	conf.Server.EnableSharing = false
	conf.Server.EnableInsightsCollector = false
	conf.Server.DevActivityPanel = false
	conf.Server.DevAutoCreateAdminPassword = ""
	conf.Server.DevAutoLoginUsername = ""
	conf.Server.EnableLogRedacting = true
	conf.Server.DataFolder = conf.NewDir(roots["data"])
	conf.Server.CacheFolder = conf.NewDir(roots["cache"])
	conf.Server.MusicFolder = roots["music"]
	conf.Server.DbPath = filepath.Join(roots["data"], "oracle.db")

	closeDB := db.Init(ctx)
	if closeDB == nil {
		return exitRuntime, errors.New("db.Init returned no closer")
	}
	defer closeDB()

	prov.SQLiteRuntime = sqliteRuntime(ctx)

	// auth.Init mirrors production startup (server/server.go:41). No token is minted here.
	ds := persistence.New(db.Db())
	auth.Init(ds)

	nativeRouter := nativeapi.New(
		ds,
		nil,                                     // share: route not registered, because EnableSharing is false
		playlists.NewPlaylists(ds, nil),         // imgUpload only used by upload handlers
		nil,                                     // insights: only dereferenced inside the /insights handler
		core.NewLibrary(ds, nil, nil, nil, nil), // scanner/watcher/broker/pluginManager inert
		core.NewUser(ds, nil),                   // pluginManager only used by the delete path
		nil,                                     // maintenance: only dereferenced inside /missing DELETE
		nil,                                     // pluginManager: only dereferenced inside /plugin handlers
		nil,                                     // artwork uploader: only dereferenced inside image upload handlers
		nil,                                     // external provider: only dereferenced inside refresh handlers
		nil,                                     // quickConnect: route not registered, because quickconnect.Enabled() is false
	)

	// Mirror production mounting without a listener: defaultMiddlewares include JWTVerifier
	// (server/server.go:170-185) and cmd/root.go:128 mounts via Server.MountRouter, i.e.
	// r.Group(func(r){ r.Mount(path.Join(BasePath, URLPathNativeAPI), subRouter) })
	// (server/server.go:50-56). So native routes are reachable at /api/... as specced.
	outer := chi.NewRouter()
	outer.Use(server.JWTVerifier)
	outer.Group(func(r chi.Router) {
		r.Mount(path.Join(conf.Server.BasePath, consts.URLPathNativeAPI), nativeRouter)
	})
	handler := outer

	results := make([]observed, 0, len(spec.Requests))
	for _, c := range spec.Requests {
		o, err := runCase(ctx, handler, c, spec.Behavior)
		if err != nil {
			return exitRuntime, fmt.Errorf("case %s: %w", c.CaseID, err)
		}
		results = append(results, o)
	}
	sort.Slice(results, func(i, j int) bool { return results[i].CaseID < results[j].CaseID })

	// Write into the owned staging area (exclusive), NOT the destination.
	staging := filepath.Join(root, stagingName)
	if err := os.Mkdir(staging, 0700); err != nil {
		return exitRuntime, fmt.Errorf("create staging dir: %w", err)
	}
	casesDir := filepath.Join(staging, casesName)
	if err := os.Mkdir(casesDir, 0700); err != nil {
		return exitRuntime, fmt.Errorf("create cases dir: %w", err)
	}

	ids := make([]string, 0, len(results))
	for _, r := range results {
		ids = append(ids, r.CaseID)
		blob, err := json.MarshalIndent(r, "", "  ")
		if err != nil {
			return exitRuntime, fmt.Errorf("marshal case %s: %w", r.CaseID, err)
		}
		if err := writeExclusive(filepath.Join(casesDir, r.CaseID+".json"), append(blob, '\n'), 0600); err != nil {
			return exitRuntime, fmt.Errorf("write case %s: %w", r.CaseID, err)
		}
	}

	m := manifest{
		Kind:                "api00-native-corpus-first-slice",
		Origin:              origin,
		Behavior:            behavior,
		ProductionBinaryRun: false,
		ListenerOpened:      false,
		Authenticated:       false,
		NetworkUsed:         false,
		CorpusScope:         spec.CorpusScope,
		Provenance:          prov,
		Cases:               ids,
	}
	mblob, err := json.MarshalIndent(m, "", "  ")
	if err != nil {
		return exitRuntime, fmt.Errorf("marshal manifest: %w", err)
	}
	if err := writeExclusive(filepath.Join(staging, "manifest.json"), append(mblob, '\n'), 0600); err != nil {
		return exitRuntime, fmt.Errorf("write manifest: %w", err)
	}

	// Report first, then gate. A verdict mismatch is a hard failure BEFORE promotion:
	// nothing reaches the destination and the owned root is removed by main's cleanup.
	fmt.Printf("ORIGIN: %s\n", origin)
	fmt.Printf("BEHAVIOR: %s\n", behavior)
	fmt.Printf("BINARY/LISTENER/AUTH/NETWORK: false false false false\n")
	fmt.Printf("PINNED_GO_REF: %s tree=%s go_build_inputs=%s\n", prov.SourceRef, prov.SourceRefTree, prov.GoSourceSHA256)
	fmt.Printf("INFO_WORKTREE (not in artifact): head=%s tree=%s offline=%s\n", worktreeLabel, worktreeTreeLabel, offlineInfo)
	fmt.Printf("CASES: %d\n", len(ids))
	fmt.Printf("CASES_SHA256: %s\n", digestOf(casesDir))
	fmt.Printf("FULL_SHA256 (compared artifact): %s\n", digestOfTree(staging))
	fmt.Printf("GO: %s  SQLITE: %s  PLAT: %s/%s  BUILD_TAGS: %s\n",
		prov.GoVersion, prov.SQLiteRuntime, prov.OS, prov.Arch, prov.BuildTags)
	for _, r := range results {
		fmt.Printf("%-46s %3d %-26s %s\n", r.CaseID, r.Status, r.Headers["content-type"], r.Verdict)
	}

	mismatched := make([]string, 0)
	for _, r := range results {
		if strings.HasPrefix(r.Verdict, "OBSERVED-DIFFERS") {
			mismatched = append(mismatched, r.CaseID+" ("+r.Verdict+")")
		}
	}
	if len(mismatched) > 0 {
		return exitVerdict, fmt.Errorf("verdict mismatch on %d/%d case(s) before promotion: %s",
			len(mismatched), len(results), strings.Join(mismatched, "; "))
	}

	// Promote by exclusive copy into the destination, which was verified absent above.
	if err := os.Mkdir(outDir, 0700); err != nil {
		return exitRefuse, fmt.Errorf("destination appeared during run, refusing: %w", err)
	}
	if err := copyTree(staging, outDir); err != nil {
		return exitRuntime, fmt.Errorf("promote artifact: %w", err)
	}
	fmt.Printf("PROMOTED: %s\n", outDir)
	return exitOK, nil
}

func runCase(ctx context.Context, handler http.Handler, c caseSpec, behavior string) (observed, error) {
	var body io.Reader
	if c.Body != nil {
		body = strings.NewReader(*c.Body)
	}
	req := httptest.NewRequest(c.Method, c.Path, body)
	req = req.WithContext(ctx)
	if c.Auth == "invalid_token" {
		req.Header.Set(authHeader, unsignedBearer)
	}
	for k, v := range c.Headers {
		req.Header.Set(k, v)
	}
	if req.Header.Get("Accept") == "" {
		req.Header.Set("Accept", "application/json")
	}

	rec := newRecorder()
	handler.ServeHTTP(rec, req)
	headers := rec.snapshotHeaders()

	raw := redact(rec.rec.Body.Bytes())
	o := observed{
		CaseID:        c.CaseID,
		Origin:        origin,
		BehaviorClass: cmpOr(c.BehaviorClass, behavior),
		RequestMethod: c.Method,
		RequestPath:   c.Path,
		AuthMode:      c.Auth,
		Status:        rec.rec.Code,
		Headers:       flattenHeaders(headers),
		BodyKind:      classifyBody(raw),
		BodyLength:    len(raw),
		BodySHA256:    hexSHA256(raw),
		Body:          string(raw),
		Provenance:    c.Provenance,
	}
	o.Verdict = verdict(o, c)
	return o, nil
}

func cmpOr(a, b string) string {
	if a != "" {
		return a
	}
	return b
}

// writeOnceRecorder wraps httptest.ResponseRecorder and snapshots response headers at the
// first write: UpdateLastAccessMiddleware mutates Header() AFTER the handler writes on the
// first call per user, and on a real socket those late additions cannot reach a flushed
// response. Snapshotting at write time reproduces what an actual client receives.
type writeOnceRecorder struct {
	rec     *httptest.ResponseRecorder
	snap    http.Header
	written bool
}

func newRecorder() *writeOnceRecorder { return &writeOnceRecorder{rec: httptest.NewRecorder()} }

func (w *writeOnceRecorder) Header() http.Header {
	if w.written {
		return w.snap
	}
	return w.rec.Header()
}

func (w *writeOnceRecorder) Write(b []byte) (int, error) { w.snapshot(); return w.rec.Write(b) }
func (w *writeOnceRecorder) WriteHeader(code int)        { w.snapshot(); w.rec.WriteHeader(code) }
func (w *writeOnceRecorder) Flush()                      { w.snapshot(); w.rec.Flush() }

func (w *writeOnceRecorder) snapshot() {
	if w.written {
		return
	}
	w.written = true
	w.snap = cloneHeader(w.rec.Header())
}

func (w *writeOnceRecorder) snapshotHeaders() http.Header {
	if w.snap != nil {
		return w.snap
	}
	return w.rec.Header()
}

func cloneHeader(h http.Header) http.Header {
	out := http.Header{}
	for k, v := range h {
		out[k] = append([]string(nil), v...)
	}
	return out
}

func flattenHeaders(h http.Header) map[string]string {
	out := map[string]string{}
	keys := make([]string, 0, len(h))
	for k := range h {
		keys = append(keys, strings.ToLower(k))
	}
	sort.Strings(keys)
	for _, k := range keys {
		v := strings.Join(h[canonicalKey(k)], ", ")
		if secretHeaders[k] {
			v = "<redacted>"
		}
		out[k] = v
	}
	return out
}

var secretHeaders = map[string]bool{
	"authorization": true, "x-nd-authorization": true, "cookie": true,
	"set-cookie": true, "x-auth-token": true,
}

func canonicalKey(lower string) string {
	b := []byte(lower)
	for i := range b {
		if i == 0 || b[i-1] == '-' {
			if b[i] >= 'a' && b[i] <= 'z' {
				b[i] -= 'a' - 'A'
			}
		}
	}
	return string(b)
}

var redactions = []*regexp.Regexp{
	regexp.MustCompile(`(?i)"(token|password|secret|api_?key|access_?token|refresh_?token|jwt_?secret)"\s*:\s*"[^"]*"`),
	regexp.MustCompile(`(?i)(subsonicSalt|subsonicToken)\s*:\s*"[^"]*"`),
}

// redact replaces credential-shaped JSON field values with typed placeholders; status,
// content type and the remaining bytes are preserved unchanged.
func redact(b []byte) []byte {
	out := b
	for _, re := range redactions {
		out = re.ReplaceAllFunc(out, func(m []byte) []byte {
			i := valueStart(m)
			if i < 0 {
				return m
			}
			return append(append([]byte{}, m[:i]...), []byte(`"<redacted>"`)...)
		})
	}
	return out
}

func valueStart(m []byte) int {
	for i := len(m) - 1; i >= 0; i-- {
		if m[i] == '"' {
			for j := i - 1; j >= 0; j-- {
				if m[j] == ':' {
					return j + 1
				}
			}
		}
	}
	return -1
}

func classifyBody(b []byte) string {
	t := strings.TrimSpace(string(b))
	if t == "" {
		return "empty"
	}
	if t[0] == '{' || t[0] == '[' {
		var v any
		if err := json.Unmarshal([]byte(t), &v); err == nil {
			switch v.(type) {
			case map[string]any:
				return "json-object"
			case []any:
				return "json-array"
			}
		}
		return "json-like"
	}
	return "text"
}

func verdict(o observed, c caseSpec) string {
	var problems []string
	if c.ExpectStatus != 0 && o.Status != c.ExpectStatus {
		problems = append(problems, fmt.Sprintf("status %d, spec expected %d", o.Status, c.ExpectStatus))
	}
	if c.ExpectContentType != "" && !ctMatches(o.Headers["content-type"], c.ExpectContentType) {
		problems = append(problems, fmt.Sprintf("content-type %q, spec expected %q", o.Headers["content-type"], c.ExpectContentType))
	}
	keys := make([]string, 0, len(c.ExpectHeaders))
	for k := range c.ExpectHeaders {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	for _, k := range keys {
		got := o.Headers[strings.ToLower(k)]
		if got != c.ExpectHeaders[k] {
			problems = append(problems, fmt.Sprintf("header %s %q, spec expected %q", k, got, c.ExpectHeaders[k]))
		}
	}
	if len(problems) == 0 {
		return "observed-matches-source-derived-spec"
	}
	return "OBSERVED-DIFFERS-FROM-SPEC: " + strings.Join(problems, "; ")
}

func ctMatches(got, want string) bool {
	return strings.EqualFold(strings.TrimSpace(strings.Split(got, ";")[0]), strings.TrimSpace(strings.Split(want, ";")[0]))
}

// safeCaseID: lowercase ascii basename, no separators, no dot-leading, bounded length.
var safeCaseID = regexp.MustCompile(`^[a-z0-9][a-z0-9._-]{0,63}$`)

func validateCaseIDs(reqs []caseSpec) error {
	seen := map[string]bool{}
	for _, c := range reqs {
		if c.CaseID == "" || c.CaseID == "." || c.CaseID == ".." {
			return fmt.Errorf("unsafe case_id %q", c.CaseID)
		}
		if strings.ContainsAny(c.CaseID, `/\`) || filepath.Base(c.CaseID) != c.CaseID {
			return fmt.Errorf("case_id %q is not a safe basename", c.CaseID)
		}
		if !safeCaseID.MatchString(c.CaseID) {
			return fmt.Errorf("case_id %q is not a safe basename (need %s)", c.CaseID, safeCaseID.String())
		}
		if seen[c.CaseID] {
			return fmt.Errorf("duplicate case_id %q", c.CaseID)
		}
		seen[c.CaseID] = true
	}
	return nil
}

// writeExclusive never truncates or replaces an existing file.
func writeExclusive(name string, data []byte, perm os.FileMode) error {
	f, err := os.OpenFile(name, os.O_WRONLY|os.O_CREATE|os.O_EXCL, perm)
	if err != nil {
		return err
	}
	if _, err := f.Write(data); err != nil {
		_ = f.Close()
		return err
	}
	return f.Close()
}

// copyTree copies staging into a fresh destination using exclusive writes only.
func copyTree(from, to string) error {
	entries, err := os.ReadDir(from)
	if err != nil {
		return err
	}
	for _, e := range entries {
		src := filepath.Join(from, e.Name())
		dst := filepath.Join(to, e.Name())
		if e.IsDir() {
			if err := os.Mkdir(dst, 0700); err != nil {
				return err
			}
			if err := copyTree(src, dst); err != nil {
				return err
			}
			continue
		}
		if e.Type()&os.ModeSymlink != 0 {
			return fmt.Errorf("refusing to copy symlink %s", src)
		}
		b, err := os.ReadFile(src)
		if err != nil {
			return err
		}
		if err := writeExclusive(dst, b, 0600); err != nil {
			return err
		}
	}
	return nil
}

func sqliteRuntime(ctx context.Context) string {
	rows, err := db.Db().QueryContext(ctx, "SELECT sqlite_version()")
	if err != nil {
		return "unknown"
	}
	defer rows.Close()
	var v string
	if rows.Next() {
		if err := rows.Scan(&v); err != nil {
			return "unknown"
		}
	}
	return v
}

// worktreeInfo returns the current HEAD and its tree hash for informational stdout only.
// They are deliberately NOT serialized into the compared artifact: HEAD moves on Rust- and
// docs-only commits, and a corpus that recorded it would drift for reasons unrelated to the
// Go behavior being captured.
func worktreeInfo(repoRoot string) (head, tree string) {
	if h, err := git(repoRoot, "rev-parse", "HEAD"); err == nil {
		head = h
	} else {
		head = "unknown"
	}
	if t, err := git(repoRoot, "rev-parse", "HEAD^{tree}"); err == nil {
		tree = t
	} else {
		tree = "unknown"
	}
	return head, tree
}

func git(repoRoot string, args ...string) (string, error) {
	cmd := exec.Command("git", append([]string{"-C", repoRoot}, args...)...)
	out, err := cmd.Output()
	if err != nil {
		return "", err
	}
	return strings.TrimSpace(string(out)), nil
}

// buildInputPaths are the compared Go build inputs: the local packages reachable from the
// captured native-router chain, the module graph, and the embedded-input directories. The
// set is intentionally conservative (whole tracked trees, not a transitive import graph), so
// any change able to alter captured behavior — embedded assets and migrations included — is
// detected rather than silently accepted.
var buildInputPaths = []string{
	"go.mod", "go.sum",
	"server", "core", "persistence", "conf", "consts", "model", "utils", "db", "adapters",
	"resources", "ui", "log", "scheduler", "plugins",
}

// resolveSourceRef validates the pin instead of copying it: the named ORIGINAL Go ref must
// resolve to a commit in this repository. It pins the captured Go build inputs, not HEAD.
func resolveSourceRef(repoRoot, ref string) (string, error) {
	if ref == "" {
		return "", errors.New("spec has no source_ref")
	}
	if _, err := git(repoRoot, "rev-parse", "--verify", "-q", ref+"^{commit}"); err != nil {
		return "", fmt.Errorf("pinned Go source ref %q does not resolve to a commit in this repository", ref)
	}
	return ref, nil
}

// verifyBuildInputs fails closed unless the actual on-disk build inputs match the pinned
// ref: no tracked file under the compared paths may differ from it, and no untracked or
// ignored file may exist inside them (a stray source or embedded input changes the build).
func verifyBuildInputs(repoRoot, ref string) error {
	// -z keeps paths with spaces intact (the snapshot directory has many of them).
	out, err := git(repoRoot, append([]string{"diff", "-z", "--name-only", ref, "--"}, buildInputPaths...)...)
	if err != nil {
		return fmt.Errorf("compare on-disk build inputs against pinned ref %s: %w", ref, err)
	}
	if changed := nulSplit(out); len(changed) > 0 {
		return fmt.Errorf("on-disk Go build inputs differ from pinned ref %s: %d file(s) differ, first: %s",
			ref, len(changed), changed[0])
	}
	for _, args := range [][]string{
		append([]string{"ls-files", "-z", "--others", "--"}, buildInputPaths...),
		append([]string{"ls-files", "-z", "--others", "--ignored", "--exclude-standard", "--"}, buildInputPaths...),
	} {
		out, err := git(repoRoot, args...)
		if err != nil {
			return fmt.Errorf("list untracked build inputs under the compared paths: %w", err)
		}
		if extra := nulSplit(out); len(extra) > 0 {
			return fmt.Errorf("untracked build input under the compared paths (rejected): %s", extra[0])
		}
	}
	return nil
}

// buildInputDigest produces a stable digest of the compared build inputs. The entry list
// always comes from the pinned ref's tree; blob hashes come from that commit's objects when
// fromRef is true and from the actual files on disk otherwise. Both sides hash path + mode
// + git blob hash, so the digests are directly comparable and any modified, deleted, added
// or type-changed file makes them differ.
func buildInputDigest(repoRoot, ref string, fromRef bool) (string, error) {
	entries, err := buildInputEntries(repoRoot, ref)
	if err != nil {
		return "", err
	}
	h := sha256.New()
	for _, e := range entries {
		blob := e.hash
		if !fromRef {
			blob, err = diskBlobHash(repoRoot, e.path, e.mode)
			if err != nil {
				return "", err
			}
		}

		fmt.Fprintf(h, "%s\x00%s\x00%s\x00", e.path, e.mode, blob)
	}
	return hex.EncodeToString(h.Sum(nil)), nil
}

type buildEntry struct{ path, mode, hash string }

// buildInputEntries lists the compared build inputs from the pinned ref's tree, using -z so
// paths containing spaces are parsed exactly. verifyBuildInputs has already proven the
// working tree matches this tree, so the same list describes the on-disk inputs.
func buildInputEntries(repoRoot, ref string) ([]buildEntry, error) {
	out, err := git(repoRoot, append([]string{"ls-tree", "-r", "-z", ref, "--"}, buildInputPaths...)...)
	if err != nil {
		return nil, fmt.Errorf("list build inputs from ref %s: %w", ref, err)
	}
	entries := make([]buildEntry, 0, 4096)
	for _, rec := range nulSplit(out) {
		meta, path, ok := strings.Cut(rec, "\t")
		if !ok {
			return nil, fmt.Errorf("unparsable ls-tree record %q", rec)
		}
		fields := strings.Fields(meta)
		if len(fields) != 3 {
			return nil, fmt.Errorf("unparsable ls-tree metadata %q", meta)
		}
		entries = append(entries, buildEntry{path: path, mode: fields[0], hash: fields[2]})
	}
	if len(entries) < 1000 {
		return nil, fmt.Errorf("implausibly few build inputs (%d) — compared path set is wrong", len(entries))
	}
	return entries, nil
}

// nulSplit splits NUL-separated git output into sorted, non-empty records.
func nulSplit(s string) []string {
	out := make([]string, 0, 16)
	for _, r := range strings.Split(s, "\x00") {
		if r != "" {
			out = append(out, r)
		}
	}
	sort.Strings(out)
	return out
}

// diskBlobHash hashes actual on-disk bytes as a git blob object, matching the tracked mode,
// so it is comparable with the ref side's ls-tree hash.
func diskBlobHash(repoRoot, path, mode string) (string, error) {
	var content []byte
	switch mode {
	case "120000":
		target, err := os.Readlink(filepath.Join(repoRoot, path))
		if err != nil {
			return "", fmt.Errorf("read build input %s: %w", path, err)
		}
		content = []byte(target)
	case "100644", "100755":
		b, err := os.ReadFile(filepath.Join(repoRoot, path))
		if err != nil {
			return "", fmt.Errorf("read build input %s: %w", path, err)
		}
		content = b
	default:
		return "", fmt.Errorf("unsupported build-input mode %s for %s", mode, path)
	}
	h := sha1.New()
	fmt.Fprintf(h, "blob %d\x00", len(content))
	h.Write(content)
	return hex.EncodeToString(h.Sum(nil)), nil
}

// modulePins records exact versions and h1 checksums of modules on the captured path.
func modulePins(goMod, goSum string) []string {
	want := []string{
		"github.com/deluan/rest",
		"github.com/go-chi/chi/v5",
		"github.com/go-chi/jwtauth/v5",
		"github.com/lestrrat-go/jwx/v3",
		"github.com/mattn/go-sqlite3",
		"github.com/pressly/goose/v3",
	}
	var out []string
	for _, name := range want {
		for _, line := range strings.Split(goMod, "\n") {
			t := strings.TrimSpace(line)
			if strings.HasPrefix(t, name+" ") {
				out = append(out, "go.mod: "+t)
				break
			}
		}
		for _, line := range strings.Split(goSum, "\n") {
			if strings.HasPrefix(line, name+" ") && strings.Contains(line, " h1:") {
				out = append(out, "go.sum: "+strings.TrimSpace(line))
				break
			}
		}
	}
	return out
}

func syntheticConfig(roots map[string]string) string {
	return fmt.Sprintf(`# Synthetic throwaway config for an isolated in-process capture.
# No credentials; never used outside this owned temp root. No listener is opened.
MusicFolder = %q
DataFolder = %q
CacheFolder = %q
LogLevel = "error"
Address = "127.0.0.1"
Port = 4533
EnableExternalServices = false
Scanner.Enabled = false
Scanner.ScanOnStartup = false
Plugins.Enabled = false
Prometheus.Enabled = false
EnableSharing = false
EnableInsightsCollector = false
DevActivityPanel = false
SessionTimeout = "10m"
`, roots["music"], roots["data"], roots["cache"])
}

func readSpec(name string) (specFile, error) {
	var s specFile
	b, err := os.ReadFile(name)
	if err != nil {
		return s, err
	}
	if err := json.Unmarshal(b, &s); err != nil {
		return s, fmt.Errorf("parse %s: %w", name, err)
	}
	return s, nil
}

// normalizePaths removes run-specific temp paths so pinned digests are comparable.
func normalizePaths(s string) string {
	return regexp.MustCompile(`"[^"]*`+tempPrefix+`[^"]*"`).ReplaceAllStringFunc(s, func(m string) string {
		switch {
		case strings.HasSuffix(m, `/home"`):
			return `"<HOME>"`
		case strings.HasSuffix(m, `/config"`):
			return `"<CONFIG>"`
		case strings.HasSuffix(m, `/music"`):
			return `"<MUSIC>"`
		case strings.HasSuffix(m, `/data"`):
			return `"<DATA>"`
		case strings.HasSuffix(m, `/cache"`):
			return `"<CACHE>"`
		}
		return `"<TMP>"`
	})
}

// digestOf hashes sorted case bytes only.
func digestOf(casesDir string) string {
	names, err := sortedNames(casesDir)
	if err != nil {
		return "unavailable"
	}
	h := sha256.New()
	for _, n := range names {
		b, err := os.ReadFile(filepath.Join(casesDir, n))
		if err != nil {
			return "unavailable"
		}
		h.Write([]byte(n))
		h.Write([]byte{0})
		h.Write(b)
		h.Write([]byte{0})
	}
	return hex.EncodeToString(h.Sum(nil))
}

// digestOfTree hashes the FULL artifact (manifest + all cases), recursively.
func digestOfTree(dir string) string {
	h := sha256.New()
	err := filepath.WalkDir(dir, func(p string, d os.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if d.IsDir() {
			return nil
		}
		rel, err := filepath.Rel(dir, p)
		if err != nil {
			return err
		}
		b, err := os.ReadFile(p)
		if err != nil {
			return err
		}
		h.Write([]byte(rel))
		h.Write([]byte{0})
		h.Write(b)
		h.Write([]byte{0})
		return nil
	})
	if err != nil {
		return "unavailable"
	}
	return hex.EncodeToString(h.Sum(nil))
}

func sortedNames(dir string) ([]string, error) {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil, err
	}
	names := make([]string, 0, len(entries))
	for _, e := range entries {
		names = append(names, e.Name())
	}
	sort.Strings(names)
	return names, nil
}

// allowedEnv keeps runtime essentials only; nothing config- or credential-bearing.
func allowedEnv() map[string]string {
	keep := map[string]string{}
	for _, kv := range os.Environ() {
		k, v, ok := strings.Cut(kv, "=")
		if !ok {
			continue
		}
		switch k {
		case "PATH", "TMPDIR", "USER", "LOGNAME", "SHELL", "TERM", "LANG", "LC_ALL",
			"GOROOT", "GOPATH", "GOCACHE", "GOMODCACHE", "SSL_CERT_FILE", "SSL_CERT_DIR":
			keep[k] = v
		}
	}
	return keep
}

func offlineLabel() string {
	proxy := lookup("GOPROXY")
	toolchain := lookup("GOTOOLCHAIN")
	if proxy == "" {
		proxy = "(unset)"
	}
	if toolchain == "" {
		toolchain = "(unset)"
	}
	return "GOPROXY=" + proxy + " GOTOOLCHAIN=" + toolchain + " GOFLAGS=" + cmpOr(lookup("GOFLAGS"), "(unset)")
}

func lookup(k string) string {
	for _, kv := range os.Environ() {
		if v, ok := strings.CutPrefix(kv, k+"="); ok {
			return v
		}
	}
	return ""
}

func hexSHA256(b []byte) string {
	s := sha256.Sum256(b)
	return hex.EncodeToString(s[:])
}
