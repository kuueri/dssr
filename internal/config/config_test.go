package config

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

const valid = `listen: 127.0.0.1:8080
index_path: /var/lib/dssr/index
admin_socket: /run/dssr/admin.sock
timezone: Asia/Jakarta
clients:
  - name: library-chatbot
    token_sha256: 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08
sources:
  - id: repo
    name: Example Repository
    oai_url: https://repository.example.org/oai/request
    metadata_prefix: dim
    schedule: "0 3 * * 1"
`

func write(t *testing.T, s string) string {
	t.Helper()
	p := filepath.Join(t.TempDir(), "dssr.yaml")
	if err := os.WriteFile(p, []byte(s), 0o600); err != nil {
		t.Fatal(err)
	}
	return p
}

func TestLoadValid(t *testing.T) {
	c, err := Load(write(t, valid))
	if err != nil {
		t.Fatal(err)
	}
	if c.Location.String() != "Asia/Jakarta" || c.Sources[0].ID != "repo" || len(c.Clients) != 1 {
		t.Fatalf("unexpected config: %+v", c)
	}
}

func TestLoadDefaultPrefix(t *testing.T) {
	c, err := Load(write(t, strings.Replace(valid, "    metadata_prefix: dim\n", "", 1)))
	if err != nil {
		t.Fatal(err)
	}
	if p := c.Sources[0].MetadataPrefix; p != "oai_dc" {
		t.Fatalf("metadata_prefix = %q, want oai_dc", p)
	}
}

func TestLoadNoSources(t *testing.T) {
	head, _, _ := strings.Cut(valid, "sources:")
	c, err := Load(write(t, head+"# sources:\n#   - id: repo\n"))
	if err != nil {
		t.Fatal(err)
	}
	if len(c.Sources) != 0 {
		t.Fatalf("sources = %+v, want none", c.Sources)
	}
}

func TestLoadRejects(t *testing.T) {
	cases := map[string]struct{ old, new, want string }{
		"unknown key":  {"timezone:", "timezon:", "not found"},
		"bad hash":     {"9f86d081", "zz86d081", "token_sha256"},
		"bad schedule": {`"0 3 * * 1"`, `"every monday"`, "schedule"},
		"bad prefix":   {"metadata_prefix: dim", "metadata_prefix: marcxml", "metadata_prefix"},
		"bad id":       {"id: repo", "id: Repo:1", "source id"},
		"bad url":      {"https://repository", "ftp://repository", "oai_url"},
		"long socket":  {"/run/dssr/", "/run/" + strings.Repeat("d", 100) + "/", "admin_socket"},
	}
	for name, c := range cases {
		t.Run(name, func(t *testing.T) {
			_, err := Load(write(t, strings.Replace(valid, c.old, c.new, 1)))
			if err == nil || !strings.Contains(err.Error(), c.want) {
				t.Fatalf("want error containing %q, got %v", c.want, err)
			}
		})
	}
}
