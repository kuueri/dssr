package main

import (
	"os"
	"path/filepath"
	"testing"
)

func TestUninstallPaths(t *testing.T) {
	path := filepath.Join(t.TempDir(), "dssr.yaml")
	index, socket, err := uninstallPaths(path)
	if err != nil {
		t.Fatalf("missing config: %v", err)
	}
	if want := filepath.Join(stateDir, "index"); index != want {
		t.Errorf("index = %q, want %q", index, want)
	}
	if want := filepath.Join(runtimeDir, "admin.sock"); socket != want {
		t.Errorf("admin_socket = %q, want %q", socket, want)
	}

	body := "index_path: /srv/dssr/index\nadmin_socket: /srv/dssr/admin.sock\nsources: []\n"
	if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
	index, socket, err = uninstallPaths(path)
	if err != nil {
		t.Fatalf("custom config: %v", err)
	}
	if index != "/srv/dssr/index" || socket != "/srv/dssr/admin.sock" {
		t.Errorf("got index %q and socket %q", index, socket)
	}

	if err := os.WriteFile(path, []byte("index_path: [\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, _, err := uninstallPaths(path); err == nil {
		t.Fatal("malformed config: want error")
	}
}

func TestCheckRemovable(t *testing.T) {
	for _, path := range []string{"/", "/etc", "/var/lib", "/usr/local", "/etc/systemd/system", "relative", ""} {
		if err := checkRemovable(path); err == nil {
			t.Errorf("checkRemovable(%q) = nil, want error", path)
		}
	}
	for _, path := range []string{"/var/lib/dssr", "/var/lib/dssr/index", "/srv/dssr/index", "/run/dssr"} {
		if err := checkRemovable(path); err != nil {
			t.Errorf("checkRemovable(%q) = %v, want nil", path, err)
		}
	}
}

func TestMailboxPaths(t *testing.T) {
	path := filepath.Join(t.TempDir(), "login.defs")
	if err := os.WriteFile(path, []byte("# mail\nMAIL_DIR /srv/mail\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	got := mailboxPaths(path, "dssr")
	if len(got) != 1 || got[0] != "/srv/mail/dssr" {
		t.Errorf("custom MAIL_DIR: got %q", got)
	}

	if err := os.WriteFile(path, []byte("MAIL_DIR mail\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	want := []string{"/var/mail/dssr", "/var/spool/mail/dssr"}
	got = mailboxPaths(path, "dssr")
	if len(got) != len(want) || got[0] != want[0] || got[1] != want[1] {
		t.Errorf("relative MAIL_DIR: got %q, want %q", got, want)
	}

	got = mailboxPaths(filepath.Join(t.TempDir(), "missing"), "dssr")
	if len(got) != len(want) || got[0] != want[0] || got[1] != want[1] {
		t.Errorf("missing login.defs: got %q, want %q", got, want)
	}
}
