package main

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"go.yaml.in/yaml/v3"

	"github.com/kuueri/dssr/internal/config"
)

const tokenConfig = `listen: 127.0.0.1:8080
index_path: /var/lib/dssr/index
admin_socket: /run/dssr/admin.sock
timezone: Asia/Jakarta

clients:
  - name: default
    token_sha256: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa

# Add each OAI-PMH source, then run: sudo systemctl restart dssr
# sources:
#   - id: repo
`

func clientsOf(t *testing.T, b []byte) []config.Client {
	t.Helper()
	var c struct {
		Clients []config.Client `yaml:"clients"`
	}
	if err := yaml.Unmarshal(b, &c); err != nil {
		t.Fatal(err)
	}
	return c.Clients
}

func TestSetClientToken(t *testing.T) {
	b := strings.Repeat("b", 64)
	out, replaced, err := setClientToken([]byte(tokenConfig), "default", b)
	if err != nil || !replaced {
		t.Fatalf("replace: replaced %t, %v", replaced, err)
	}
	if c := clientsOf(t, out); len(c) != 1 || c[0].TokenSHA256 != b {
		t.Fatalf("replace: clients %+v", c)
	}
	for _, want := range []string{"\n  - name: default\n    token_sha256: ", "#   - id: repo"} {
		if !strings.Contains(string(out), want) {
			t.Errorf("replace: output lacks %q:\n%s", want, out)
		}
	}

	c64 := strings.Repeat("c", 64)
	out, replaced, err = setClientToken(out, "digilib", c64)
	if err != nil || replaced {
		t.Fatalf("add: replaced %t, %v", replaced, err)
	}
	if c := clientsOf(t, out); len(c) != 2 || c[1].Name != "digilib" || c[1].TokenSHA256 != c64 {
		t.Fatalf("add: clients %+v", c)
	}

	for _, in := range []string{"listen: 127.0.0.1:8080\n", "listen: 127.0.0.1:8080\nclients:\n"} {
		out, _, err := setClientToken([]byte(in), "default", b)
		if err != nil {
			t.Fatalf("%q: %v", in, err)
		}
		if c := clientsOf(t, out); len(c) != 1 || c[0].TokenSHA256 != b {
			t.Fatalf("%q: clients %+v", in, c)
		}
	}

	if _, _, err := setClientToken([]byte("clients: nope\n"), "default", b); err == nil {
		t.Fatal("clients as a scalar: want error")
	}
}

func TestReplaceConfig(t *testing.T) {
	path := filepath.Join(t.TempDir(), "dssr.yaml")
	if err := os.WriteFile(path, []byte(tokenConfig), 0o640); err != nil {
		t.Fatal(err)
	}
	if err := replaceConfig(path, []byte(tokenConfig+"bogus: 1\n")); err == nil {
		t.Fatal("invalid config: want error")
	}
	if b, _ := os.ReadFile(path); string(b) != tokenConfig {
		t.Fatal("invalid config replaced the file")
	}

	next := strings.Replace(tokenConfig, "name: default", "name: library", 1)
	if err := replaceConfig(path, []byte(next)); err != nil {
		t.Fatal(err)
	}
	fi, err := os.Stat(path)
	if err != nil || fi.Mode().Perm() != 0o640 {
		t.Fatalf("mode %v, %v", fi.Mode(), err)
	}
	if b, _ := os.ReadFile(path); string(b) != next {
		t.Fatalf("content %q", b)
	}
	if left, _ := filepath.Glob(filepath.Join(filepath.Dir(path), ".dssr.yaml.*")); len(left) != 0 {
		t.Fatalf("temporary files left: %v", left)
	}
}
