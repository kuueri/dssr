package oai

import (
	"context"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"
)

func serve(t *testing.T, h http.HandlerFunc) *Client {
	t.Helper()
	ts := httptest.NewServer(h)
	t.Cleanup(ts.Close)
	c := New(ts.URL, "dssr/test")
	c.Backoff = time.Millisecond
	return c
}

func TestListRecordsFollowsResumptionToken(t *testing.T) {
	var queries []string
	c := serve(t, func(w http.ResponseWriter, r *http.Request) {
		queries = append(queries, r.URL.RawQuery)
		if r.UserAgent() != "dssr/test" {
			t.Errorf("user agent %q", r.UserAgent())
		}
		if r.URL.Query().Get("resumptionToken") == "dim////100" {
			http.ServeFile(w, r, "testdata/listrecords-2.xml")
			return
		}
		http.ServeFile(w, r, "testdata/listrecords-1.xml")
	})
	var recs []Record
	date, err := c.ListRecords(context.Background(), "dim", "2026-10-25T05:00:00Z", func(p []Record) error {
		recs = append(recs, p...)
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	if date != "2026-10-26T05:00:00Z" {
		t.Errorf("date %q, want the first responseDate", date)
	}
	if len(recs) != 3 || !recs[1].Header.Deleted() || recs[0].Header.Deleted() {
		t.Fatalf("unexpected records: %+v", recs)
	}
	if got := recs[0].Header.SetSpecs; len(got) != 2 || got[1] != "col_123456789_2" {
		t.Errorf("setSpecs %v", got)
	}
	if !strings.Contains(string(recs[0].Metadata.XML), "wisata ziarah") {
		t.Errorf("metadata not captured")
	}
	if want := "from=2026-10-25T05%3A00%3A00Z&metadataPrefix=dim&verb=ListRecords"; queries[0] != want {
		t.Errorf("first query %q, want %q", queries[0], want)
	}
}

func TestListRecordsNoRecordsMatch(t *testing.T) {
	c := serve(t, func(w http.ResponseWriter, r *http.Request) {
		http.ServeFile(w, r, "testdata/norecords.xml")
	})
	date, err := c.ListRecords(context.Background(), "dim", "", func([]Record) error {
		t.Fatal("no page expected")
		return nil
	})
	if err != nil || date != "2026-10-26T06:00:00Z" {
		t.Fatalf("date %q, err %v", date, err)
	}
}

func TestRetryAfter503(t *testing.T) {
	calls := 0
	c := serve(t, func(w http.ResponseWriter, r *http.Request) {
		calls++
		if calls == 1 {
			w.Header().Set("Retry-After", "1")
			w.WriteHeader(http.StatusServiceUnavailable)
			return
		}
		http.ServeFile(w, r, "testdata/norecords.xml")
	})
	start := time.Now()
	if _, err := c.ListRecords(context.Background(), "dim", "", func([]Record) error { return nil }); err != nil {
		t.Fatal(err)
	}
	if calls != 2 || time.Since(start) < time.Second {
		t.Fatalf("calls %d after %v, want 2 calls after at least 1s", calls, time.Since(start))
	}
}

func TestGivesUpAfterRetries(t *testing.T) {
	calls := 0
	c := serve(t, func(w http.ResponseWriter, r *http.Request) {
		calls++
		w.WriteHeader(http.StatusBadGateway)
	})
	if _, err := c.ListRecords(context.Background(), "dim", "", func([]Record) error { return nil }); err == nil {
		t.Fatal("want error")
	}
	if calls != 4 {
		t.Fatalf("calls %d, want 1 + 3 retries", calls)
	}
}

func TestNoRetryOn404(t *testing.T) {
	calls := 0
	c := serve(t, func(w http.ResponseWriter, r *http.Request) {
		calls++
		http.NotFound(w, r)
	})
	if _, err := c.ListSets(context.Background()); err == nil || calls != 1 {
		t.Fatalf("err %v after %d calls", err, calls)
	}
}

func TestListSets(t *testing.T) {
	c := serve(t, func(w http.ResponseWriter, r *http.Request) {
		http.ServeFile(w, r, "testdata/listsets.xml")
	})
	sets, err := c.ListSets(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if sets["com_123456789_1"] != "Fakultas Ekonomi" || len(sets) != 2 {
		t.Fatalf("sets %v", sets)
	}
}
