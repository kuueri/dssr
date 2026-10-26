package server

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"net/url"
	"path/filepath"
	"testing"

	"github.com/kuueri/dssr/internal/config"
	"github.com/kuueri/dssr/internal/harvest"
	"github.com/kuueri/dssr/internal/index"
	"github.com/kuueri/dssr/internal/record"
)

func setup(t *testing.T) *Server {
	t.Helper()
	idx, err := index.Open(filepath.Join(t.TempDir(), "index"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { idx.Close() })
	err = idx.Apply([]record.Record{
		{ID: "repo:123456789/56", Source: "repo", Title: "Analisis potensi wisata ziarah", Issued: "2020", Type: []string{"Thesis"}},
	}, nil, "1")
	if err != nil {
		t.Fatal(err)
	}
	sum := sha256.Sum256([]byte("secret"))
	cfg := &config.Config{
		Clients: []config.Client{{Name: "test", TokenSHA256: hex.EncodeToString(sum[:])}},
		Sources: []config.Source{{ID: "repo", Schedule: "0 3 * * 1"}},
	}
	h := harvest.New(context.Background(), idx, cfg.Sources, "dssr/test")
	return New(idx, h, cfg, "test")
}

func do(t *testing.T, h http.Handler, method, target, token string) *httptest.ResponseRecorder {
	t.Helper()
	req := httptest.NewRequest(method, target, nil)
	if token != "" {
		req.Header.Set("Authorization", "Bearer "+token)
	}
	w := httptest.NewRecorder()
	h.ServeHTTP(w, req)
	return w
}

func TestAPI(t *testing.T) {
	api := setup(t).API()
	cases := []struct {
		target, token string
		status        int
	}{
		{"/healthz", "", 200},
		{"/openapi.json", "", 200},
		{"/v1/search?q=wisata", "", 401},
		{"/v1/search?q=wisata", "wrong", 401},
		{"/v1/search?q=wisata", "secret", 200},
		{"/v1/search", "secret", 400},
		{"/v1/search?q=wisata&limit=51", "secret", 400},
		{"/v1/search?q=wisata&page=0", "secret", 400},
		{"/v1/search?q=wisata&page=21&limit=50", "secret", 400},
		{"/v1/search?q=wisata&page=20&limit=50", "secret", 200},
		{"/v1/search?q=wisata&year=20-21", "secret", 400},
		{"/v1/search?q=wisata&year=2024-2020", "secret", 400},
		{"/v1/search?q=wisata&sort=oldest", "secret", 400},
		{"/v1/records/repo/123456789/56", "", 401},
		{"/v1/records/repo/123456789/56", "secret", 200},
		{"/v1/records/repo/123456789/99", "secret", 404},
	}
	for _, c := range cases {
		w := do(t, api, "GET", c.target, c.token)
		if w.Code != c.status {
			t.Errorf("%s with %q: status %d, want %d: %s", c.target, c.token, w.Code, c.status, w.Body)
		}
		if w.Code >= 400 && w.Header().Get("Content-Type") != "application/problem+json" {
			t.Errorf("%s: content type %q", c.target, w.Header().Get("Content-Type"))
		}
	}
}

func TestSearchBody(t *testing.T) {
	w := do(t, setup(t).API(), "GET", "/v1/search?"+url.Values{"q": {`"wisata ziarah"`}, "type": {"Thesis"}, "year": {"2020"}}.Encode(), "secret")
	var res index.Result
	if err := json.NewDecoder(w.Body).Decode(&res); err != nil {
		t.Fatal(err)
	}
	if res.Total != 1 || res.Results[0].ID != "repo:123456789/56" || res.Results[0].Issued != "2020" || res.Facets["type"][0].Value != "Thesis" {
		t.Fatalf("result %+v", res)
	}
}

func TestOpenAPIIsJSON(t *testing.T) {
	var v map[string]any
	if err := json.Unmarshal(openapi, &v); err != nil || v["openapi"] != "3.1.0" {
		t.Fatalf("openapi.json: %v", err)
	}
}

func TestAdmin(t *testing.T) {
	admin := setup(t).Admin()
	w := do(t, admin, "GET", "/status", "")
	var st Status
	if err := json.NewDecoder(w.Body).Decode(&st); err != nil || len(st.Sources) != 1 || st.Sources[0].Docs != 1 {
		t.Fatalf("status %+v, %v", st, err)
	}
	if w := do(t, admin, "POST", "/harvest?source=nope", ""); w.Code != 404 {
		t.Fatalf("unknown source status %d", w.Code)
	}
	if w := do(t, admin, "GET", "/records/repo/123456789/56", ""); w.Code != 200 {
		t.Fatalf("admin record status %d", w.Code)
	}
}
