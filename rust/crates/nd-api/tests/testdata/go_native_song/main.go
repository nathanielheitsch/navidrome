//go:build netgo && sqlite_fts5

// Command go_native_song captures authenticated song reads from Navidrome's real
// native router, using an isolated migrated database and scanner-generated rows.
// It uses only httptest recorders; no production server or listener is started.
package main

import (
	"context"
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
	"path/filepath"
	"regexp"
	"runtime"
	"sort"
	"strings"
	"testing/fstest"

	"github.com/go-chi/chi/v5"
	"github.com/navidrome/navidrome/conf"
	"github.com/navidrome/navidrome/consts"
	"github.com/navidrome/navidrome/core"
	"github.com/navidrome/navidrome/core/auth"
	"github.com/navidrome/navidrome/core/metrics"
	"github.com/navidrome/navidrome/core/playlists"
	"github.com/navidrome/navidrome/core/storage/storagetest"
	"github.com/navidrome/navidrome/db"
	"github.com/navidrome/navidrome/log"
	"github.com/navidrome/navidrome/model"
	"github.com/navidrome/navidrome/persistence"
	"github.com/navidrome/navidrome/scanner"
	"github.com/navidrome/navidrome/server"
	"github.com/navidrome/navidrome/server/events"
	"github.com/navidrome/navidrome/server/nativeapi"
)

const (
	origin     = "in-process Go native router capture (net/http/httptest recorder; no listener, network, or production binary)"
	stableTime = "2024-01-02T03:04:05Z"
)

type caseResult struct {
	CaseID          string            `json:"case_id"`
	RequestMethod   string            `json:"request_method"`
	RequestPath     string            `json:"request_path"`
	AuthMode        string            `json:"auth_mode"`
	Status          int               `json:"status"`
	ResponseHeaders map[string]string `json:"response_headers"`
	Body            string            `json:"body"`
	BodyLength      int               `json:"body_length"`
	BodySHA256      string            `json:"body_sha256"`
	Behavior        string            `json:"behavior"`
}

type manifest struct {
	Origin                       string            `json:"origin"`
	Authenticated                bool              `json:"authenticated"`
	ProductionBinaryRun          bool              `json:"production_binary_run"`
	ListenerOpened               bool              `json:"listener_opened"`
	NetworkUsed                  bool              `json:"network_used"`
	ScannerProducedRows          int               `json:"scanner_produced_rows"`
	SyntheticIdentity            string            `json:"synthetic_identity"`
	DeterministicTransformations []string          `json:"deterministic_transformations"`
	Provenance                   map[string]string `json:"provenance"`
	Cases                        []string          `json:"cases"`
}

type listedPackage struct {
	Dir             string
	Module          *struct{ Path string }
	GoFiles         []string
	CgoFiles        []string
	CompiledGoFiles []string
	CFiles          []string
	CXXFiles        []string
	MFiles          []string
	HFiles          []string
	FFiles          []string
	SFiles          []string
	SwigFiles       []string
	SwigCXXFiles    []string
	SysoFiles       []string
	EmbedFiles      []string
}

func main() { os.Exit(run()) }

func run() int {
	out := flag.String("out", "", "fresh, nonexistent artifact directory")
	repo := flag.String("repo-root", ".", "repository root used for fixture provenance")
	compiledInputs := flag.String("compiled-inputs", "", "go list -deps -json output for this harness")
	flag.Parse()
	if *out == "" || *compiledInputs == "" || flag.NArg() != 0 {
		fail("usage: go_native_song --out NEW_DIR --compiled-inputs GO_LIST_JSON [--repo-root ROOT]")
	}
	root, err := filepath.Abs(*repo)
	if err != nil {
		fail("resolve repository root")
	}
	deps, err := filepath.Abs(*compiledInputs)
	if err != nil {
		fail("resolve compiled input inventory")
	}
	dest, err := filepath.Abs(*out)
	if err != nil {
		fail("resolve destination")
	}
	if _, err := os.Lstat(dest); err == nil || !errors.Is(err, os.ErrNotExist) {
		fail("destination exists or cannot be checked; refusing to overwrite")
	}

	owned, err := os.MkdirTemp("", "go-native-song.")
	if err != nil {
		fail("create private temporary root")
	}
	defer func() {
		if err := os.RemoveAll(owned); err != nil {
			fmt.Fprintln(os.Stderr, "cleanup failed for owned temporary root")
		}
	}()
	if err := os.Chmod(owned, 0700); err != nil {
		fmt.Fprintln(os.Stderr, "go_native_song: isolate temp root failed")
		return 1
	}
	for _, name := range []string{"home", "config", "data", "cache", "music"} {
		if err := os.Mkdir(filepath.Join(owned, name), 0700); err != nil {
			fmt.Fprintln(os.Stderr, "go_native_song: create isolated directory failed")
			return 1
		}
	}
	if err := os.Chdir(filepath.Join(owned, "home")); err != nil {
		fmt.Fprintln(os.Stderr, "go_native_song: enter isolated directory failed")
		return 1
	}
	isolateEnvironment(filepath.Join(owned, "home"))
	if err := capture(root, dest, owned, deps); err != nil {
		fmt.Fprintln(os.Stderr, "go_native_song: capture failed; no response or private path emitted")
		return 1
	}
	return 0
}

func capture(repo, dest, root, depsPath string) error {
	ctx := context.Background()
	cfg := fmt.Sprintf("MusicFolder = %q\nDataFolder = %q\nCacheFolder = %q\nLogLevel = %q\nAddress = %q\nPort = 4533\nEnableExternalServices = false\nScanner.Enabled = false\nScanner.ScanOnStartup = false\nPlugins.Enabled = false\nPrometheus.Enabled = false\nEnableSharing = false\nEnableInsightsCollector = false\nDevActivityPanel = false\nSessionTimeout = %q\n", "fixtureone:///music", filepath.Join(root, "data"), filepath.Join(root, "cache"), "error", "127.0.0.1", "10m")
	cfgPath := filepath.Join(root, "config", "navidrome.toml")
	if err := os.WriteFile(cfgPath, []byte(cfg), 0600); err != nil {
		return fmt.Errorf("write synthetic config: %w", err)
	}
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
	conf.Server.DevExternalScanner = false
	conf.Server.DataFolder = conf.NewDir(filepath.Join(root, "data"))
	conf.Server.CacheFolder = conf.NewDir(filepath.Join(root, "cache"))
	conf.Server.DbPath = filepath.Join(root, "data", "song-oracle.db")
	log.SetLevel(log.LevelError)
	log.SetOutput(io.Discard)
	closeDB := db.Init(ctx)
	defer closeDB()

	ds := persistence.New(db.Db())
	auth.Init(ds)
	storagetest.Register("fixtureone", makeSyntheticLibrary("Visible Fixture Song", "Visible Album", 123))
	storagetest.Register("fixturetwo", makeSyntheticLibrary("Restricted Fixture Song", "Restricted Album", 99))
	defaultLibrary, err := ds.Library().Get(ctx, model.DefaultLibraryID)
	if err != nil {
		return fmt.Errorf("load configured default library: %w", err)
	}
	defaultLibrary.Name = "Fixture Library One"
	if err := ds.Library().Put(ctx, defaultLibrary); err != nil {
		return fmt.Errorf("configure default fixture library: %w", err)
	}
	secondLibrary := model.Library{Name: "Fixture Library Two", Path: "fixturetwo:///music"}
	if err := ds.Library().Put(ctx, &secondLibrary); err != nil {
		return fmt.Errorf("create second synthetic library: %w", err)
	}

	admin := model.User{ID: "fixture-authorized", UserName: "fixture-authorized", Name: "Authorized Fixture", IsAdmin: true, NewPassword: "disposable-only"}
	regular := model.User{ID: "fixture-restricted", UserName: "fixture-restricted", Name: "Restricted Fixture", IsAdmin: false, NewPassword: "disposable-only"}
	for _, user := range []*model.User{&admin, &regular} {
		if err := ds.User().Put(ctx, user); err != nil {
			return fmt.Errorf("create synthetic identity: %w", err)
		}
	}
	if err := ds.User().SetUserLibraries(ctx, admin.ID, []int{1, 2}); err != nil {
		return fmt.Errorf("grant authorized identity: %w", err)
	}
	if err := ds.User().SetUserLibraries(ctx, regular.ID, []int{1}); err != nil {
		return fmt.Errorf("grant restricted identity: %w", err)
	}

	// Scanner-generated rows in both libraries; no datastore rows are hand-written.
	scanCtx := auth.WithAdminUser(ctx, ds)
	s := scanner.New(scanCtx, ds, events.NoopBroker(), playlists.NewPlaylists(ds, nil), metrics.NewNoopInstance())
	if warnings, err := s.ScanAll(scanCtx, true); err != nil {
		return fmt.Errorf("real scanner scan failed (%d warnings): %w", len(warnings), err)
	}
	all, err := ds.MediaFile().GetAll(context.Background())
	if err != nil {
		return fmt.Errorf("read scanner-produced rows: %w", err)
	}
	if len(all) < 2 {
		return fmt.Errorf("non-vacuity: scanner produced %d rows, want tracks in both libraries", len(all))
	}
	var visible, denied model.MediaFile
	for _, song := range all {
		if song.LibraryID == 1 {
			visible = song
		}
		if song.LibraryID == 2 {
			denied = song
		}
	}
	if visible.ID == "" || denied.ID == "" {
		return fmt.Errorf("non-vacuity: scanner did not produce a row in each library")
	}

	native := nativeapi.New(ds, nil, playlists.NewPlaylists(ds, nil), nil, core.NewLibrary(ds, nil, nil, nil, nil), core.NewUser(ds, nil), nil, nil, nil, nil, nil)
	router := chi.NewRouter()
	router.Use(server.JWTVerifier)
	router.Group(func(r chi.Router) { r.Mount(pathJoin(conf.Server.BasePath, consts.URLPathNativeAPI), native) })

	// Signed sessions are minted for disposable DB identities and pass real middleware.
	tokAdmin, err := auth.CreateToken(&admin)
	if err != nil {
		return fmt.Errorf("mint disposable authorized session: %w", err)
	}
	tokRegular, err := auth.CreateToken(&regular)
	if err != nil {
		return fmt.Errorf("mint disposable restricted session: %w", err)
	}
	cases := []caseResult{}
	appendCase := func(id, path, token, mode, behavior string) (caseResult, error) {
		result, err := captureRequest(router, path, token, mode, behavior)
		if err == nil {
			result.CaseID = id
			cases = append(cases, result)
		}
		return result, err
	}
	base := pathJoin(conf.Server.BasePath, consts.URLPathNativeAPI)

	// Verify authenticated middleware and genuinely visible scanned detail first.
	positive, err := appendCase("song_detail_authorized_control", base+"/song/"+visible.ID, tokAdmin, "signed-session", "authorized scanner-produced detail control")
	if err != nil {
		return err
	}
	if positive.Status < 200 || positive.Status >= 300 || !strings.Contains(positive.Body, visible.ID) {
		return fmt.Errorf("authenticated precondition failed: observed status %d without scanned ID", positive.Status)
	}

	list, err := appendCase("song_list_authorized", base+"/song?_sort=title&_order=ASC", tokAdmin, "signed-session", "authorized scanner-produced list sorted by title")
	if err != nil {
		return err
	}
	if list.Status < 200 || list.Status >= 300 {
		return fmt.Errorf("authorized list did not succeed: observed status %d", list.Status)
	}
	var listed []struct {
		ID string `json:"id"`
	}
	if err := json.Unmarshal([]byte(list.Body), &listed); err != nil {
		return fmt.Errorf("decode observed song list: %w", err)
	}
	expectedIDs := []string{visible.ID, denied.ID}
	actualIDs := make([]string, 0, len(listed))
	for _, row := range listed {
		actualIDs = append(actualIDs, row.ID)
	}
	sort.Strings(expectedIDs)
	sort.Strings(actualIDs)
	if len(listed) != len(expectedIDs) || strings.Join(actualIDs, "\x00") != strings.Join(expectedIDs, "\x00") {
		return fmt.Errorf("authorized list returned unexpected scanner row inventory (%d rows)", len(listed))
	}
	if list.ResponseHeaders["x-total-count"] != fmt.Sprint(len(expectedIDs)) {
		return fmt.Errorf("authorized list total-count header disagrees with observed rows")
	}

	if _, err := appendCase("song_detail_authorized", base+"/song/"+visible.ID, tokAdmin, "signed-session", "authorized scanner-produced detail"); err != nil {
		return err
	}
	if _, err := appendCase("song_detail_missing", base+"/song/no-such-song-api00", tokAdmin, "signed-session", "authenticated missing ID"); err != nil {
		return err
	}
	noSession, err := appendCase("song_detail_no_session", base+"/song/"+visible.ID, "", "no-session", "song detail without credentials")
	if err != nil {
		return err
	}
	if noSession.Status < 400 {
		return fmt.Errorf("no-session request was not rejected: observed status %d", noSession.Status)
	}

	granted, err := appendCase("regular_user_granted_success", base+"/song/"+visible.ID, tokRegular, "signed-session", "regular user reads row in granted library")
	if err != nil {
		return err
	}
	if granted.Status < 200 || granted.Status >= 300 || !strings.Contains(granted.Body, visible.ID) {
		return fmt.Errorf("regular user's granted-library read failed: observed status %d", granted.Status)
	}

	// Compare the same library-2 scanner row under authorized and restricted identities.
	allowed, err := captureRequest(router, base+"/song/"+denied.ID, tokAdmin, "signed-session", "authorized same-record control")
	if err != nil {
		return err
	}
	if allowed.Status < 200 || allowed.Status >= 300 || !strings.Contains(allowed.Body, denied.ID) {
		return fmt.Errorf("same-record authorized control failed: observed status %d", allowed.Status)
	}
	allowed.CaseID = "same_record_visible_authorized"
	cases = append(cases, allowed)
	if _, err := appendCase("song_detail_denied_library", base+"/song/"+denied.ID, tokRegular, "signed-session", "same record outside granted library"); err != nil {
		return err
	}

	for i := range cases {
		cases[i].RequestPath = strings.ReplaceAll(cases[i].RequestPath, visible.ID, "fixture-song-library-one")
		cases[i].RequestPath = strings.ReplaceAll(cases[i].RequestPath, denied.ID, "fixture-song-library-two")
		cases[i].Body = strings.ReplaceAll(cases[i].Body, visible.ID, "fixture-song-library-one")
		cases[i].Body = strings.ReplaceAll(cases[i].Body, denied.ID, "fixture-song-library-two")
		body := []byte(cases[i].Body)
		cases[i].BodyLength = len(body)
		sum := sha256.Sum256(body)
		cases[i].BodySHA256 = hex.EncodeToString(sum[:])
	}
	sort.Slice(cases, func(i, j int) bool { return cases[i].CaseID < cases[j].CaseID })

	prov, err := provenance(repo, depsPath)
	if err != nil {
		return err
	}
	m := manifest{
		Origin:              origin,
		Authenticated:       true,
		ScannerProducedRows: len(all),
		SyntheticIdentity:   "disposable database users; credentials and bearer tokens not recorded",
		ProductionBinaryRun: false,
		ListenerOpened:      false,
		NetworkUsed:         false,
		DeterministicTransformations: []string{
			"JSON createdAt/updatedAt/birthTime string values are replaced with fixed UTC 2024-01-02T03:04:05Z after recorder capture",
			"scanner-assigned media row IDs are replaced with fixture-song-library-one/two in paths and bodies so independent scratch databases compare; unrelated IDs are retained",
			"file size is fixed in synthetic test tags; status, response headers, content type, remaining body bytes and JSON formatting are preserved",
			"credential response headers are omitted; bearer tokens/passwords are not recorded",
		},
		Provenance: prov,
	}
	for _, c := range cases {
		m.Cases = append(m.Cases, c.CaseID)
	}
	if err := os.Mkdir(dest, 0700); err != nil {
		return fmt.Errorf("create fresh destination: %w", err)
	}
	caseDir := filepath.Join(dest, "cases")
	if err := os.Mkdir(caseDir, 0700); err != nil {
		return err
	}
	for _, c := range cases {
		b, err := json.MarshalIndent(c, "", "  ")
		if err != nil {
			return err
		}
		if err := os.WriteFile(filepath.Join(caseDir, c.CaseID+".json"), append(b, '\n'), 0600); err != nil {
			return err
		}
	}
	b, err := json.MarshalIndent(m, "", "  ")
	if err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(dest, "manifest.json"), append(b, '\n'), 0600); err != nil {
		return err
	}
	fmt.Printf("captured %d cases; scanner rows=%d; go=%s sqlite=%s\n", len(cases), len(all), runtime.Version(), sqliteVersion(ctx))
	for _, c := range cases {
		fmt.Printf("%s %d %s\n", c.CaseID, c.Status, c.ResponseHeaders["content-type"])
	}
	return nil
}

func captureRequest(handler http.Handler, requestPath, token, mode, behavior string) (caseResult, error) {
	req := httptest.NewRequest(http.MethodGet, requestPath, nil)
	if token != "" {
		req.Header.Set(consts.UIAuthorizationHeader, "Bearer "+token)
	}
	recorder := httptest.NewRecorder()
	handler.ServeHTTP(recorder, req)
	body := deterministicBody(recorder.Body.Bytes())
	headers := make(map[string]string)
	for key, vals := range recorder.Header() {
		lower := strings.ToLower(key)
		if lower == "authorization" || lower == "cookie" || lower == "set-cookie" || lower == "x-nd-authorization" || lower == "x-auth-token" {
			continue
		}
		headers[lower] = strings.Join(vals, ", ")
	}
	sum := sha256.Sum256(body)
	return caseResult{
		RequestMethod:   http.MethodGet,
		RequestPath:     requestPath,
		AuthMode:        mode,
		Status:          recorder.Code,
		ResponseHeaders: headers,
		Body:            string(body),
		BodyLength:      len(body),
		BodySHA256:      hex.EncodeToString(sum[:]),
		Behavior:        behavior,
	}, nil
}

var timestampFields = regexp.MustCompile(`("(?:createdAt|updatedAt|birthTime)"\s*:\s*)"[^"]*"`)

func deterministicBody(raw []byte) []byte {
	return timestampFields.ReplaceAll(raw, []byte(`${1}"`+stableTime+`"`))
}

func makeSyntheticLibrary(title, album string, bpm int) *storagetest.FakeFS {
	fixed := "2024-01-02T03:04:05Z"
	base := storagetest.Template(map[string]any{"albumartist": "Fixture Artist", "artist": "Fixture Artist", "album": album, "year": 2024})
	fs := &storagetest.FakeFS{}
	fs.SetFiles(fstest.MapFS{
		"Fixture Artist/" + album + "/01 Track.mp3": base(storagetest.Track(1, title, map[string]any{"duration": 199, "bpm": bpm, "_size": int64(8_000_000)}, storagetest.ModTime(fixed), storagetest.BirthTime(fixed))),
	})
	return fs
}

func isolateEnvironment(home string) {
	allowed := map[string]string{}
	for _, key := range []string{"PATH", "TMPDIR", "GOROOT", "GOPATH", "GOCACHE", "GOMODCACHE", "SSL_CERT_FILE", "SSL_CERT_DIR"} {
		if value, ok := os.LookupEnv(key); ok {
			allowed[key] = value
		}
	}
	os.Clearenv()
	for key, value := range allowed {
		_ = os.Setenv(key, value)
	}
	_ = os.Setenv("HOME", home)
	_ = os.Setenv("GOTOOLCHAIN", "local")
	_ = os.Setenv("GOENV", "off")
	_ = os.Setenv("GOWORK", "off")
	_ = os.Setenv("GOPROXY", "off")
	_ = os.Setenv("GOSUMDB", "off")
	_ = os.Setenv("GOPRIVATE", "")
	_ = os.Setenv("GONOPROXY", "")
	_ = os.Setenv("GONOSUMDB", "")
	_ = os.Setenv("GOINSECURE", "")
	_ = os.Setenv("GOVCS", "*:off")
	_ = os.Setenv("GOFLAGS", "-mod=readonly")
}

func provenance(repo, depsPath string) (map[string]string, error) {
	paths := []string{
		"go.mod",
		"go.sum",
		"rust/crates/nd-api/tests/testdata/go_native_song/main.go",
		"rust/crates/nd-api/tests/testdata/go_native_song/capture.sh",
		"rust/crates/nd-api/tests/testdata/go_native_song/README.md",
	}
	out := map[string]string{
		"go_version":     runtime.Version(),
		"build_tags":     "netgo sqlite_fts5",
		"os":             runtime.GOOS,
		"arch":           runtime.GOARCH,
		"sqlite_runtime": sqliteVersion(context.Background()),
	}
	for _, path := range paths {
		sum, err := hashFile(filepath.Join(repo, path))
		if err != nil {
			return nil, fmt.Errorf("provenance source %s unavailable: %w", path, err)
		}
		out[path] = sum
	}
	inputs, err := compiledLocalInputs(repo, depsPath)
	if err != nil {
		return nil, err
	}
	if len(inputs) == 0 {
		return nil, fmt.Errorf("compiled local input inventory is empty")
	}
	required := []string{
		"scanner/controller.go",
		"scanner/folder_entry.go",
		"scanner/phase_1_folders.go",
		"scanner/phase_2_missing_tracks.go",
		"scanner/phase_3_refresh_albums.go",
		"persistence/sql_base_repository.go",
		"resources/mappings.yaml",
		"resources/mime_types.yaml",
		"db/migrations",
	}
	for _, requiredPath := range required {
		if requiredPath == "db/migrations" {
			found := false
			for path := range inputs {
				if strings.HasPrefix(path, "db/migrations/") {
					found = true
					break
				}
			}
			if !found {
				return nil, fmt.Errorf("compiled input inventory omits embedded migrations")
			}
			continue
		}
		if _, ok := inputs[requiredPath]; !ok {
			return nil, fmt.Errorf("compiled input inventory omits required local input %s", requiredPath)
		}
	}
	names := make([]string, 0, len(inputs))
	for path, digest := range inputs {
		out["compiled:"+path] = digest
		names = append(names, path)
	}
	sort.Strings(names)
	h := sha256.New()
	for _, path := range names {
		_, _ = h.Write([]byte(path + "\x00" + inputs[path] + "\x00"))
	}
	out["compiled_local_inventory_sha256"] = hex.EncodeToString(h.Sum(nil))
	return out, nil
}

func compiledLocalInputs(repo, depsPath string) (map[string]string, error) {
	file, err := os.Open(depsPath)
	if err != nil {
		return nil, fmt.Errorf("open compiler input listing: %w", err)
	}
	defer file.Close()
	decoder := json.NewDecoder(file)
	files := map[string]struct{}{}
	for {
		var pkg listedPackage
		if err := decoder.Decode(&pkg); err != nil {
			if errors.Is(err, io.EOF) {
				break
			}
			return nil, fmt.Errorf("decode compiler input listing: %w", err)
		}
		if pkg.Module == nil || pkg.Module.Path != "github.com/navidrome/navidrome" {
			continue
		}
		names := append([]string{}, pkg.GoFiles...)
		names = append(names, pkg.CgoFiles...)
		names = append(names, pkg.CompiledGoFiles...)
		names = append(names, pkg.CFiles...)
		names = append(names, pkg.CXXFiles...)
		names = append(names, pkg.MFiles...)
		names = append(names, pkg.HFiles...)
		names = append(names, pkg.FFiles...)
		names = append(names, pkg.SFiles...)
		names = append(names, pkg.SwigFiles...)
		names = append(names, pkg.SwigCXXFiles...)
		names = append(names, pkg.SysoFiles...)
		names = append(names, pkg.EmbedFiles...)
		for _, name := range names {
			full := name
			if !filepath.IsAbs(full) {
				full = filepath.Join(pkg.Dir, full)
			}
			relative, err := filepath.Rel(repo, full)
			if err != nil || relative == ".." || strings.HasPrefix(relative, ".."+string(filepath.Separator)) {
				return nil, fmt.Errorf("compiler input escapes repository root")
			}
			files[filepath.ToSlash(relative)] = struct{}{}
		}
	}
	out := make(map[string]string, len(files))
	for path := range files {
		digest, err := hashFile(filepath.Join(repo, filepath.FromSlash(path)))
		if err != nil {
			return nil, fmt.Errorf("hash compiled local input %s: %w", path, err)
		}
		out[path] = digest
	}
	return out, nil
}

func hashFile(path string) (string, error) {
	b, err := os.ReadFile(path)
	if err != nil {
		return "", err
	}
	sum := sha256.Sum256(b)
	return hex.EncodeToString(sum[:]), nil
}

func hashTree(root string) (string, error) {
	h := sha256.New()
	err := filepath.WalkDir(root, func(name string, entry os.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if entry.IsDir() {
			return nil
		}
		rel, err := filepath.Rel(root, name)
		if err != nil {
			return err
		}
		b, err := os.ReadFile(name)
		if err != nil {
			return err
		}
		_, _ = h.Write([]byte(filepath.ToSlash(rel) + "\x00"))
		_, _ = h.Write(b)
		_, _ = h.Write([]byte{0})
		return nil
	})
	if err != nil {
		return "", err
	}
	return hex.EncodeToString(h.Sum(nil)), nil
}

func sqliteVersion(ctx context.Context) string {
	var version string
	_ = db.Db().QueryRowContext(ctx, "select sqlite_version()").Scan(&version)
	return version
}
func pathJoin(a, b string) string {
	return strings.TrimSuffix(a, "/") + "/" + strings.TrimPrefix(b, "/")
}
func fail(message string) { fmt.Fprintln(os.Stderr, "go_native_song:", message); os.Exit(2) }
