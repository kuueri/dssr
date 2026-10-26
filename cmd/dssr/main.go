package main

import (
	"bytes"
	"cmp"
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"io/fs"
	"log/slog"
	"net"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"os/signal"
	"os/user"
	"path/filepath"
	"runtime/debug"
	"slices"
	"strings"
	"syscall"
	"time"

	"go.yaml.in/yaml/v3"

	"github.com/kuueri/dssr/internal/config"
	"github.com/kuueri/dssr/internal/harvest"
	"github.com/kuueri/dssr/internal/index"
	"github.com/kuueri/dssr/internal/server"
)

const usage = `Usage:
  dssr serve   [--config dssr.yaml]
  dssr harvest [--config dssr.yaml] [--source ID] [--full]
  dssr search  [--config dssr.yaml] <query> [--type T]... [--year 2020-2024] [--collection C]... [--language L]... [--sort newest] [--page 1] [--limit 10] [--json]
  dssr get     [--config dssr.yaml] <id> [--json]
  dssr status  [--config dssr.yaml] [--json]
  dssr eval    [--config dssr.yaml] <file>
  dssr uninstall
  dssr --version
`

func main() {
	if len(os.Args) < 2 {
		fmt.Fprint(os.Stderr, usage)
		os.Exit(2)
	}
	cmds := map[string]func([]string) error{
		"serve":     serve,
		"harvest":   harvestCmd,
		"search":    search,
		"get":       get,
		"status":    status,
		"eval":      eval,
		"uninstall": uninstall,
	}
	switch arg := os.Args[1]; {
	case arg == "--version" || arg == "-version":
		fmt.Println(version())
	case arg == "-h" || arg == "--help" || arg == "help":
		fmt.Print(usage)
	case cmds[arg] != nil:
		if err := cmds[arg](os.Args[2:]); err != nil {
			fmt.Fprintln(os.Stderr, "dssr:", err)
			os.Exit(1)
		}
	default:
		fmt.Fprint(os.Stderr, usage)
		os.Exit(2)
	}
}

var buildVersion string

func version() string {
	v := cmp.Or(buildVersion, "devel")
	bi, ok := debug.ReadBuildInfo()
	if !ok {
		return v
	}
	var rev, dirty string
	for _, s := range bi.Settings {
		switch s.Key {
		case "vcs.revision":
			rev = s.Value[:min(7, len(s.Value))]
		case "vcs.modified":
			if s.Value == "true" {
				dirty = "-dirty"
			}
		}
	}
	if rev == "" {
		return v
	}
	return v + " (" + rev + dirty + ")"
}

type multi []string

func (m *multi) String() string     { return strings.Join(*m, ",") }
func (m *multi) Set(v string) error { *m = append(*m, v); return nil }

func parse(fs *flag.FlagSet, args []string) []string {
	var pos []string
	for {
		fs.Parse(args)
		if fs.NArg() == 0 {
			return pos
		}
		pos = append(pos, fs.Arg(0))
		args = fs.Args()[1:]
	}
}

func newFlags(name string) (*flag.FlagSet, *string) {
	fs := flag.NewFlagSet(name, flag.ExitOnError)
	fs.Usage = func() { fmt.Fprint(os.Stderr, usage) }
	return fs, fs.String("config", "dssr.yaml", "configuration file")
}

func serve(args []string) error {
	fs, cfgPath := newFlags("serve")
	parse(fs, args)
	cfg, err := config.Load(*cfgPath)
	if err != nil {
		return err
	}
	slog.SetDefault(slog.New(slog.NewJSONHandler(os.Stdout, nil)))

	adminLn, err := listenUnix(cfg.AdminSocket)
	if err != nil {
		return err
	}
	defer adminLn.Close()
	apiLn, err := net.Listen("tcp", cfg.Listen)
	if err != nil {
		return err
	}
	defer apiLn.Close()
	idx, err := index.Open(cfg.IndexPath)
	if err != nil {
		return fmt.Errorf("open index %s: %w", cfg.IndexPath, err)
	}
	defer idx.Close()

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	h := harvest.New(ctx, idx, cfg.Sources, "dssr/"+version())
	srv := server.New(idx, h, cfg, version())
	api := &http.Server{Handler: srv.API(), ReadHeaderTimeout: 10 * time.Second}
	admin := &http.Server{Handler: srv.Admin(), ReadHeaderTimeout: 10 * time.Second}
	errc := make(chan error, 2)
	go func() { errc <- api.Serve(apiLn) }()
	go func() { errc <- admin.Serve(adminLn) }()

	if err := h.Schedule(cfg.Location); err != nil {
		return err
	}
	for _, s := range cfg.Sources {
		if err := h.Start(s.ID, false); err != nil {
			return err
		}
	}
	slog.Info("serving", "version", version(), "listen", cfg.Listen, "admin_socket", cfg.AdminSocket, "index", cfg.IndexPath)

	select {
	case <-ctx.Done():
	case err = <-errc:
	}
	stop()
	slog.Info("shutting down")
	shut, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	api.Shutdown(shut)
	admin.Shutdown(shut)
	h.Stop()
	return err
}

func listenUnix(path string) (net.Listener, error) {
	if c, err := net.Dial("unix", path); err == nil {
		c.Close()
		return nil, fmt.Errorf("%s: another dssr serve is running", path)
	}
	if err := os.Remove(path); err != nil && !errors.Is(err, fs.ErrNotExist) {
		return nil, err
	}
	ln, err := net.Listen("unix", path)
	if err != nil {
		return nil, err
	}
	if err := os.Chmod(path, 0o600); err != nil {
		ln.Close()
		return nil, err
	}
	return ln, nil
}

func call(cfgPath, method, path string) ([]byte, error) {
	cfg, err := config.Load(cfgPath)
	if err != nil {
		return nil, err
	}
	c := &http.Client{Transport: &http.Transport{
		DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
			var d net.Dialer
			return d.DialContext(ctx, "unix", cfg.AdminSocket)
		},
	}}
	req, err := http.NewRequest(method, "http://dssr"+path, nil)
	if err != nil {
		return nil, err
	}
	res, err := c.Do(req)
	if err != nil {
		return nil, fmt.Errorf("is dssr serve running? %w", err)
	}
	defer res.Body.Close()
	body, err := io.ReadAll(res.Body)
	if err != nil {
		return nil, err
	}
	if res.StatusCode >= 400 {
		var p server.Problem
		if json.Unmarshal(body, &p) == nil && p.Detail != "" {
			return nil, errors.New(p.Detail)
		}
		return nil, fmt.Errorf("%s %s: %s", method, path, res.Status)
	}
	return body, nil
}

func harvestCmd(args []string) error {
	fs, cfgPath := newFlags("harvest")
	source := fs.String("source", "", "source ID, default all sources")
	full := fs.Bool("full", false, "harvest every record and remove the ones that are gone")
	parse(fs, args)
	v := url.Values{}
	if *source != "" {
		v.Set("source", *source)
	}
	if *full {
		v.Set("full", "true")
	}
	body, err := call(*cfgPath, http.MethodPost, "/harvest?"+v.Encode())
	if err != nil {
		return err
	}
	var res struct {
		Started []string `json:"started"`
		Full    bool     `json:"full"`
	}
	if err := json.Unmarshal(body, &res); err != nil {
		return err
	}
	kind := "incremental"
	if res.Full {
		kind = "full"
	}
	fmt.Printf("%s harvest started for %s; follow it with dssr status\n", kind, strings.Join(res.Started, ", "))
	return nil
}

func search(args []string) error {
	fs, cfgPath := newFlags("search")
	var types, collections, languages multi
	fs.Var(&types, "type", "type filter, repeatable")
	fs.Var(&collections, "collection", "collection filter, repeatable")
	fs.Var(&languages, "language", "language filter, repeatable")
	year := fs.String("year", "", "year or range, for example 2020-2024")
	sort := fs.String("sort", "relevance", "relevance or newest")
	page := fs.String("page", "1", "page number")
	limit := fs.String("limit", "10", "results per page, up to 50")
	asJSON := fs.Bool("json", false, "print JSON")
	q := strings.Join(parse(fs, args), " ")

	v := url.Values{"q": {q}, "type": types, "collection": collections, "language": languages, "sort": {*sort}, "page": {*page}, "limit": {*limit}}
	if *year != "" {
		v.Set("year", *year)
	}
	body, err := call(*cfgPath, http.MethodGet, "/search?"+v.Encode())
	if err != nil {
		return err
	}
	if *asJSON {
		_, err := os.Stdout.Write(body)
		return err
	}
	var res index.Result
	if err := json.Unmarshal(body, &res); err != nil {
		return err
	}
	fmt.Printf("%d results\n", res.Total)
	for i, h := range res.Results {
		fmt.Printf("%3d. %s", (res.Page-1)*res.Limit+i+1, h.Title)
		if h.Issued != "" {
			fmt.Printf(" (%s)", h.Issued)
		}
		fmt.Printf("\n     %s  score %.2f\n", h.ID, h.Score)
		if len(h.Creator) > 0 {
			fmt.Printf("     %s\n", strings.Join(h.Creator, "; "))
		}
	}
	return nil
}

func get(args []string) error {
	fs, cfgPath := newFlags("get")
	asJSON := fs.Bool("json", false, "print compact JSON")
	pos := parse(fs, args)
	if len(pos) != 1 {
		return errors.New("get needs exactly one record id, for example repo:123456789/56")
	}
	source, handle, ok := strings.Cut(pos[0], ":")
	if !ok {
		return fmt.Errorf("%q is not a record id like repo:123456789/56", pos[0])
	}
	body, err := call(*cfgPath, http.MethodGet, "/records/"+url.PathEscape(source)+"/"+handle)
	if err != nil {
		return err
	}
	if *asJSON {
		_, err := os.Stdout.Write(body)
		return err
	}
	var out bytes.Buffer
	if err := json.Indent(&out, body, "", "  "); err != nil {
		return err
	}
	_, err = out.WriteTo(os.Stdout)
	return err
}

func status(args []string) error {
	fs, cfgPath := newFlags("status")
	asJSON := fs.Bool("json", false, "print JSON")
	parse(fs, args)
	body, err := call(*cfgPath, http.MethodGet, "/status")
	if err != nil {
		return err
	}
	if *asJSON {
		_, err := os.Stdout.Write(body)
		return err
	}
	var st server.Status
	if err := json.Unmarshal(body, &st); err != nil {
		return err
	}
	ts := func(t time.Time) string { return t.Local().Format("2006-01-02 15:04:05") }
	fmt.Printf("dssr %s, up since %s\n", st.Version, ts(st.Started))
	for _, s := range st.Sources {
		fmt.Printf("\n%s (%s)\n", s.ID, s.Name)
		fmt.Printf("  documents  %d\n", s.Docs)
		fmt.Printf("  cursor     %s\n", cmp.Or(s.Cursor, "none"))
		if s.Next != nil {
			fmt.Printf("  next run   %s\n", ts(*s.Next))
		}
		fmt.Printf("  running    %t\n", s.Running)
		if l := s.Last; l != nil {
			kind := "incremental"
			if l.Full {
				kind = "full"
			}
			fmt.Printf("  last run   %s, %s to %s, upserted %d, deleted %d, swept %d\n", kind, ts(l.Started), ts(l.Finished), l.Upserted, l.Deleted, l.Swept)
			if l.Note != "" {
				fmt.Printf("  note       %s\n", l.Note)
			}
			if l.Error != "" {
				fmt.Printf("  error      %s\n", l.Error)
			}
		}
		if len(s.Dropped) > 0 {
			keys := make([]string, 0, len(s.Dropped))
			for k := range s.Dropped {
				keys = append(keys, k)
			}
			slices.SortFunc(keys, func(a, b string) int { return s.Dropped[b] - s.Dropped[a] })
			fmt.Println("  dropped fields since start")
			for _, k := range keys {
				fmt.Printf("    %-40s %d\n", k, s.Dropped[k])
			}
		}
	}
	return nil
}

type evalCase struct {
	Q      string `yaml:"q"`
	Filter struct {
		Type       []string `yaml:"type"`
		Year       string   `yaml:"year"`
		Collection []string `yaml:"collection"`
		Language   []string `yaml:"language"`
	} `yaml:"filter"`
	Expect []string `yaml:"expect"`
}

func eval(args []string) error {
	fs, cfgPath := newFlags("eval")
	pos := parse(fs, args)
	if len(pos) != 1 {
		return errors.New("eval needs exactly one YAML file")
	}
	b, err := os.ReadFile(pos[0])
	if err != nil {
		return err
	}
	dec := yaml.NewDecoder(bytes.NewReader(b))
	dec.KnownFields(true)
	var cases []evalCase
	if err := dec.Decode(&cases); err != nil {
		return fmt.Errorf("%s: %w", pos[0], err)
	}
	hits := 0
	for _, c := range cases {
		v := url.Values{"q": {c.Q}, "type": c.Filter.Type, "collection": c.Filter.Collection, "language": c.Filter.Language, "limit": {"10"}}
		if c.Filter.Year != "" {
			v.Set("year", c.Filter.Year)
		}
		body, err := call(*cfgPath, http.MethodGet, "/search?"+v.Encode())
		if err != nil {
			return fmt.Errorf("%q: %w", c.Q, err)
		}
		var res index.Result
		if err := json.Unmarshal(body, &res); err != nil {
			return err
		}
		rank := 0
		for i, h := range res.Results {
			if slices.Contains(c.Expect, h.ID) {
				rank = i + 1
				break
			}
		}
		if rank > 0 {
			hits++
			fmt.Printf("HIT  %2d  %s\n", rank, c.Q)
		} else {
			fmt.Printf("MISS  -  %s\n", c.Q)
		}
	}
	if len(cases) == 0 {
		return errors.New("no queries in " + pos[0])
	}
	pct := 100 * float64(hits) / float64(len(cases))
	fmt.Printf("\nhit@10: %d/%d (%.1f%%)\n", hits, len(cases), pct)
	if pct < 80 {
		return errors.New("hit@10 is below 80%")
	}
	return nil
}

const (
	unitFile   = "/etc/systemd/system/dssr.service"
	unitDropIn = "/etc/systemd/system/dssr.service.d"
	wantsFile  = "/etc/systemd/system/multi-user.target.wants/dssr.service"
	binaryFile = "/usr/local/bin/dssr"
	docDir     = "/usr/local/share/doc/dssr"
	configDir  = "/etc/dssr"
	configFile = "/etc/dssr/dssr.yaml"
	stateDir   = "/var/lib/dssr"
	runtimeDir = "/run/dssr"
)

func uninstall(args []string) error {
	flags := flag.NewFlagSet("uninstall", flag.ExitOnError)
	flags.Usage = func() { fmt.Fprint(os.Stderr, usage) }
	flags.Parse(args)
	if flags.NArg() != 0 {
		return errors.New("uninstall takes no arguments")
	}
	if os.Geteuid() != 0 {
		return errors.New("run as root: sudo dssr uninstall")
	}

	indexPath, adminSocket, err := uninstallPaths(configFile)
	if err != nil {
		fmt.Fprintf(os.Stderr, "dssr: warning: %v; removing the default paths\n", err)
	}

	systemctl, _ := exec.LookPath("systemctl")
	if systemctl != "" {
		out, err := exec.Command(systemctl, "disable", "--now", "dssr.service").CombinedOutput()
		if err != nil && exec.Command(systemctl, "is-active", "--quiet", "dssr.service").Run() == nil {
			return fmt.Errorf("stop dssr.service: %w: %s", err, strings.TrimSpace(string(out)))
		}
		if exec.Command(systemctl, "is-active", "--quiet", "dssr.service").Run() == nil {
			return errors.New("dssr.service is still active; stop it and run uninstall again")
		}
	} else {
		fmt.Fprintln(os.Stderr, "dssr: warning: systemctl not found; the service was not stopped")
	}

	var errs []error
	remove := func(path string) {
		if err := os.Remove(path); err != nil && !errors.Is(err, fs.ErrNotExist) {
			errs = append(errs, fmt.Errorf("remove %s: %w", path, err))
		}
	}
	removeTree := func(path string) {
		clean := filepath.Clean(path)
		if err := checkRemovable(clean); err != nil {
			errs = append(errs, err)
			return
		}
		if err := os.RemoveAll(clean); err != nil {
			errs = append(errs, fmt.Errorf("remove %s: %w", clean, err))
		}
	}

	if fi, err := os.Lstat(adminSocket); err == nil && fi.Mode()&fs.ModeSocket != 0 {
		if c, err := net.Dial("unix", adminSocket); err == nil {
			c.Close()
			return fmt.Errorf("%s is in use; stop dssr and run uninstall again", adminSocket)
		}
	}

	remove(wantsFile)
	remove(unitFile)
	removeTree(unitDropIn)
	if systemctl != "" {
		if out, err := exec.Command(systemctl, "daemon-reload").CombinedOutput(); err != nil {
			fmt.Fprintf(os.Stderr, "dssr: warning: systemctl daemon-reload: %v: %s\n", err, strings.TrimSpace(string(out)))
		}
		exec.Command(systemctl, "reset-failed", "dssr.service").Run()
	}

	if _, err := os.Stat(filepath.Join(indexPath, "index_meta.json")); err == nil {
		removeTree(indexPath)
	}
	removeTree(stateDir)
	if fi, err := os.Lstat(adminSocket); err == nil {
		if fi.Mode()&fs.ModeSocket == 0 {
			errs = append(errs, fmt.Errorf("%s is not a socket, leaving it", adminSocket))
		} else {
			remove(adminSocket)
		}
	}
	removeTree(runtimeDir)

	removeTree(configDir)
	removeTree(docDir)

	userRemoved := true
	if _, err := user.Lookup("dssr"); err == nil {
		if out, err := exec.Command("userdel", "dssr").CombinedOutput(); err != nil {
			userRemoved = false
			errs = append(errs, fmt.Errorf("userdel dssr: %w: %s", err, strings.TrimSpace(string(out))))
		}
	}
	if userRemoved {
		for _, path := range mailboxPaths("/etc/login.defs", "dssr") {
			fi, err := os.Lstat(path)
			if err != nil {
				continue
			}
			if !fi.Mode().IsRegular() {
				errs = append(errs, fmt.Errorf("%s is not a regular mailbox file, leaving it", path))
				continue
			}
			remove(path)
		}
	}
	if _, err := user.LookupGroup("dssr"); err == nil {
		if out, err := exec.Command("groupdel", "dssr").CombinedOutput(); err != nil {
			errs = append(errs, fmt.Errorf("groupdel dssr: %w: %s", err, strings.TrimSpace(string(out))))
		}
	}

	if len(errs) == 0 {
		remove(binaryFile)
	}

	for _, path := range []string{wantsFile, unitFile, unitDropIn, binaryFile, docDir, configFile, configDir, stateDir, runtimeDir, indexPath, adminSocket} {
		if _, err := os.Lstat(path); err == nil {
			errs = append(errs, fmt.Errorf("%s still exists", path))
		}
	}
	if _, err := user.Lookup("dssr"); err == nil {
		errs = append(errs, errors.New("user dssr still exists"))
	}
	if _, err := user.LookupGroup("dssr"); err == nil {
		errs = append(errs, errors.New("group dssr still exists"))
	}
	if len(errs) > 0 {
		return errors.Join(errs...)
	}
	fmt.Println("dssr uninstalled")
	fmt.Println("journald keeps past dssr entries in the system journal; it cannot delete one unit's history")
	return nil
}

func uninstallPaths(path string) (indexPath, adminSocket string, err error) {
	indexPath = filepath.Join(stateDir, "index")
	adminSocket = filepath.Join(runtimeDir, "admin.sock")
	b, err := os.ReadFile(path)
	if errors.Is(err, fs.ErrNotExist) {
		return indexPath, adminSocket, nil
	}
	if err != nil {
		return indexPath, adminSocket, err
	}
	var c struct {
		IndexPath   string `yaml:"index_path"`
		AdminSocket string `yaml:"admin_socket"`
	}
	if err := yaml.Unmarshal(b, &c); err != nil {
		return indexPath, adminSocket, fmt.Errorf("%s: %w", path, err)
	}
	if c.IndexPath != "" {
		indexPath = c.IndexPath
	}
	if c.AdminSocket != "" {
		adminSocket = c.AdminSocket
	}
	return indexPath, adminSocket, nil
}

func checkRemovable(path string) error {
	clean := filepath.Clean(path)
	if !filepath.IsAbs(clean) || strings.Count(clean, "/") < 2 {
		return fmt.Errorf("refusing to remove %q", path)
	}
	switch clean {
	case "/bin", "/boot", "/dev", "/etc", "/home", "/lib", "/lib64", "/media", "/mnt", "/opt", "/proc", "/root", "/run", "/sbin", "/srv", "/sys", "/tmp", "/usr", "/var",
		"/etc/systemd", "/etc/systemd/system",
		"/usr/local", "/usr/local/bin", "/usr/local/share", "/usr/share",
		"/var/lib", "/var/log", "/var/cache", "/var/spool", "/var/tmp":
		return fmt.Errorf("refusing to remove %q", path)
	}
	return nil
}

func mailboxPaths(loginDefs, name string) []string {
	var dirs []string
	if b, err := os.ReadFile(loginDefs); err == nil {
		for line := range strings.SplitSeq(string(b), "\n") {
			f := strings.Fields(line)
			if len(f) >= 2 && f[0] == "MAIL_DIR" && filepath.IsAbs(f[1]) {
				dirs = append(dirs, f[1])
			}
		}
	}
	if len(dirs) == 0 {
		dirs = []string{"/var/mail", "/var/spool/mail"}
	}
	paths := make([]string, 0, len(dirs))
	for _, dir := range dirs {
		paths = append(paths, filepath.Join(dir, name))
	}
	return paths
}
