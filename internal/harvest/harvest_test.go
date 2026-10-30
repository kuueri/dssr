package harvest

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strconv"
	"sync"
	"testing"

	"github.com/kuueri/dssr/internal/config"
	"github.com/kuueri/dssr/internal/index"
)

type fakeRec struct {
	handle  string
	title   string
	deleted bool
}

type fakeOAI struct {
	mu    sync.Mutex
	recs  []fakeRec
	empty bool
	froms []string
	hold  chan struct{}
}

const pageSize = 2

func (f *fakeOAI) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if f.hold != nil && r.URL.Query().Get("resumptionToken") != "" {
		<-f.hold
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	q := r.URL.Query()
	fmt.Fprint(w, `<?xml version="1.0" encoding="UTF-8"?><OAI-PMH xmlns="http://www.openarchives.org/OAI/2.0/"><responseDate>2026-10-26T05:00:00Z</responseDate>`)
	defer fmt.Fprint(w, `</OAI-PMH>`)
	if q.Get("verb") == "ListSets" {
		fmt.Fprint(w, `<ListSets><set><setSpec>com_1_1</setSpec><setName>Fakultas Ekonomi</setName></set></ListSets>`)
		return
	}
	start, _ := strconv.Atoi(q.Get("resumptionToken"))
	if q.Get("resumptionToken") == "" {
		f.froms = append(f.froms, q.Get("from"))
	}
	if f.empty || len(f.recs) == 0 {
		fmt.Fprint(w, `<error code="noRecordsMatch">no records</error>`)
		return
	}
	fmt.Fprint(w, `<ListRecords>`)
	end := min(start+pageSize, len(f.recs))
	for _, rec := range f.recs[start:end] {
		if rec.deleted {
			fmt.Fprintf(w, `<record><header status="deleted"><identifier>oai:example:%s</identifier><datestamp>2026-10-20T00:00:00Z</datestamp></header></record>`, rec.handle)
			continue
		}
		fmt.Fprintf(w, `<record><header><identifier>oai:example:%s</identifier><datestamp>2026-10-20T00:00:00Z</datestamp><setSpec>com_1_1</setSpec><setSpec>col_unknown</setSpec></header><metadata><dim:dim xmlns:dim="http://www.dspace.org/xmlns/dspace/dim"><dim:field mdschema="dc" element="title">%s</dim:field><dim:field mdschema="dc" element="date" qualifier="accessioned">2026-10-20</dim:field></dim:dim></metadata></record>`, rec.handle, rec.title)
	}
	next := ""
	if end < len(f.recs) {
		next = strconv.Itoa(end)
	}
	fmt.Fprintf(w, `<resumptionToken>%s</resumptionToken></ListRecords>`, next)
}

func setup(t *testing.T, f *fakeOAI) (*Harvester, *index.Index) {
	t.Helper()
	ts := httptest.NewServer(f)
	t.Cleanup(ts.Close)
	idx, err := index.Open(filepath.Join(t.TempDir(), "index"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { idx.Close() })
	src := config.Source{ID: "repo", Name: "Repo", OAIURL: ts.URL, MetadataPrefix: "dim", Schedule: "0 3 * * 1"}
	return New(context.Background(), idx, []config.Source{src}, "dssr/test"), idx
}

func count(t *testing.T, idx *index.Index) uint64 {
	t.Helper()
	n, err := idx.Count("repo")
	if err != nil {
		t.Fatal(err)
	}
	return n
}

func TestIncrementalThenFull(t *testing.T) {
	f := &fakeOAI{recs: []fakeRec{{"1/1", "Wisata ziarah", false}, {"1/2", "Pakan jerami", false}, {"1/3", "Withdrawn", true}, {"1/4", "Strategi pemasaran", false}}}
	h, idx := setup(t, f)

	run, err := h.Run("repo", false)
	if err != nil {
		t.Fatal(err)
	}
	if run.Upserted != 3 || run.Deleted != 1 || count(t, idx) != 3 {
		t.Fatalf("first run %+v, count %d", run, count(t, idx))
	}
	r, _ := idx.Get("repo:1/1")
	if r == nil || r.Title != "Wisata ziarah" || len(r.IsPartOf) != 1 || r.IsPartOf[0] != "Fakultas Ekonomi" || r.Modified != "2026-10-20T00:00:00Z" {
		t.Fatalf("record %+v", r)
	}

	f.recs = []fakeRec{{"1/2", "Pakan jerami padi", false}, {"1/1", "", true}}
	if _, err := h.Run("repo", false); err != nil {
		t.Fatal(err)
	}
	if f.froms[0] != "" || f.froms[1] != "2026-10-25" {
		t.Fatalf("from values %q", f.froms)
	}
	if r, _ := idx.Get("repo:1/2"); r.Title != "Pakan jerami padi" {
		t.Fatalf("not updated: %+v", r)
	}
	if count(t, idx) != 2 {
		t.Fatalf("count %d after delete", count(t, idx))
	}

	f.recs = []fakeRec{{"1/2", "Pakan jerami padi", false}, {"1/5", "Baru", false}}
	run, err = h.Run("repo", true)
	if err != nil {
		t.Fatal(err)
	}
	if run.Swept != 1 || count(t, idx) != 2 || f.froms[2] != "" {
		t.Fatalf("full run %+v, count %d, from %q", run, count(t, idx), f.froms[2])
	}

	st, err := h.Status()
	if err != nil {
		t.Fatal(err)
	}
	if st[0].Docs != 2 || st[0].Cursor != "2026-10-26T05:00:00Z" || st[0].Dropped["dc.date.accessioned"] != 6 || st[0].Last.Error != "" {
		t.Fatalf("status %+v", st[0])
	}
}

func TestFullSweepGuard(t *testing.T) {
	f := &fakeOAI{recs: []fakeRec{{"1/1", "a", false}, {"1/2", "b", false}, {"1/3", "c", false}}}
	h, idx := setup(t, f)
	if _, err := h.Run("repo", true); err != nil {
		t.Fatal(err)
	}
	f.empty = true
	run, err := h.Run("repo", true)
	if err != nil {
		t.Fatal(err)
	}
	if run.Swept != 0 || run.Note == "" || count(t, idx) != 3 {
		t.Fatalf("guard did not hold: %+v, count %d", run, count(t, idx))
	}
}

func TestBusyAndUnknown(t *testing.T) {
	h, _ := setup(t, &fakeOAI{})
	if _, err := h.begin("repo"); err != nil {
		t.Fatal(err)
	}
	if _, err := h.Run("repo", false); !errors.Is(err, ErrBusy) {
		t.Fatalf("want ErrBusy, got %v", err)
	}
	if err := h.Start("nope", false); !errors.Is(err, ErrUnknownSource) {
		t.Fatalf("want ErrUnknownSource, got %v", err)
	}
}

func TestCancelKeepsCursor(t *testing.T) {
	f := &fakeOAI{recs: []fakeRec{{"1/1", "a", false}, {"1/2", "b", false}, {"1/3", "c", false}}}
	h, idx := setup(t, f)
	if err := idx.SetMeta("cursor:repo", "2026-01-01T00:00:00Z"); err != nil {
		t.Fatal(err)
	}
	f.hold = make(chan struct{})
	t.Cleanup(func() { close(f.hold) })
	if err := h.Start("repo", false); err != nil {
		t.Fatal(err)
	}
	if running, err := h.Cancel("repo"); !running || err != nil {
		t.Fatalf("cancel running %t, %v", running, err)
	}
	h.Stop()
	st, _ := h.Status()
	if st[0].Running || st[0].Last.Error != ErrCancelled.Error() || st[0].Cursor != "2026-01-01T00:00:00Z" {
		t.Fatalf("status after cancel %+v, last %+v", st[0], st[0].Last)
	}
	if running, _ := h.Cancel("repo"); running {
		t.Fatal("cancel reports a finished harvest as running")
	}
	if _, err := h.Cancel("nope"); !errors.Is(err, ErrUnknownSource) {
		t.Fatalf("want ErrUnknownSource, got %v", err)
	}
}

func TestSuspendSkipsSchedule(t *testing.T) {
	f := &fakeOAI{recs: []fakeRec{{"1/1", "a", false}}}
	h, idx := setup(t, f)
	if err := h.Suspend("repo", true); err != nil {
		t.Fatal(err)
	}
	if err := h.StartScheduled("repo"); !errors.Is(err, ErrSuspended) {
		t.Fatalf("want ErrSuspended, got %v", err)
	}
	st, _ := h.Status()
	if st[0].Suspended == "" {
		t.Fatalf("status %+v", st[0])
	}
	if _, err := h.Run("repo", false); err != nil || count(t, idx) != 1 {
		t.Fatalf("manual harvest while suspended: %v, count %d", err, count(t, idx))
	}
	if err := h.Suspend("repo", false); err != nil {
		t.Fatal(err)
	}
	if err := h.StartScheduled("repo"); err != nil {
		t.Fatal(err)
	}
	h.Stop()
	if err := h.Suspend("nope", true); !errors.Is(err, ErrUnknownSource) {
		t.Fatalf("want ErrUnknownSource, got %v", err)
	}
}

func TestFailedRunKeepsCursor(t *testing.T) {
	f := &fakeOAI{recs: []fakeRec{{"1/1", "a", false}}}
	h, idx := setup(t, f)
	if _, err := h.Run("repo", false); err != nil {
		t.Fatal(err)
	}
	if err := idx.SetMeta("cursor:repo", "2026-01-01T00:00:00Z"); err != nil {
		t.Fatal(err)
	}
	f.recs = []fakeRec{{"1/1", "<broken", false}}
	if _, err := h.Run("repo", false); err == nil {
		t.Fatal("want parse error")
	}
	if c, _ := idx.Meta("cursor:repo"); c != "2026-01-01T00:00:00Z" {
		t.Fatalf("cursor advanced to %q after a failed run", c)
	}
}
