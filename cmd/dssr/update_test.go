package main

import (
	"fmt"
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestNewer(t *testing.T) {
	for _, c := range []struct {
		a, b string
		want bool
	}{
		{"0.2.0", "0.1.9", true},
		{"0.10.0", "0.9.0", true},
		{"1.0.0", "", true},
		{"0.1.0", "0.1.0", false},
		{"0.1.0", "0.2.0", false},
	} {
		if got := newer(c.a, c.b); got != c.want {
			t.Errorf("newer(%q, %q) = %t, want %t", c.a, c.b, got, c.want)
		}
	}
}

func TestLatestRelease(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/ok":
			fmt.Fprint(w, `{"tag_name":"0.2.0"}`)
		case "/prefixed":
			fmt.Fprint(w, `{"tag_name":"v0.2.0"}`)
		default:
			http.NotFound(w, r)
		}
	}))
	defer srv.Close()

	if tag, err := latestRelease(srv.URL + "/ok"); err != nil || tag != "0.2.0" {
		t.Fatalf("latestRelease = %q, %v; want 0.2.0", tag, err)
	}
	if _, err := latestRelease(srv.URL + "/prefixed"); err == nil {
		t.Error("tag with v: want error")
	}
	if _, err := latestRelease(srv.URL + "/missing"); err == nil {
		t.Error("no release: want error")
	}
}
