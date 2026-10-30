package index

import (
	"fmt"
	"path/filepath"
	"strings"
	"testing"

	"github.com/kuueri/dssr/internal/record"
)

var fixtures = []record.Record{
	{
		ID: "repo:1/1", Source: "repo", Title: "Analisis potensi wisata ziarah Malang Raya",
		Creator: []string{"Rahma, Siti"}, Issued: "2020", Subject: []string{"wisata ziarah"},
		Abstract: []string{"Penelitian ini menganalisis potensi wisata ziarah dan pendapatan daerah."},
		Type:     []string{"Thesis"}, Language: []string{"id"}, IsPartOf: []string{"Fakultas Ekonomi"},
	},
	{
		ID: "repo:1/2", Source: "repo", Title: "Pengaruh pakan jerami padi terhadap bobot sapi",
		Creator: []string{"Hidayat, Arif"}, Issued: "2018-03-01", Subject: []string{"pakan ternak"},
		Contributor: []record.Contributor{{Name: "Santoso, Budi", Role: "advisor"}},
		Type:        []string{"Thesis"}, Language: []string{"id"}, IsPartOf: []string{"Fakultas Peternakan"},
	},
	{
		ID: "repo:1/3", Source: "repo", Title: "Strategi pemasaran wisata halal",
		Creator: []string{"Putri, Ayu"}, Issued: "2023", Subject: []string{"pemasaran"},
		Type: []string{"Article"}, Language: []string{"en"}, IsPartOf: []string{"Fakultas Ekonomi"},
	},
}

func open(t *testing.T) *Index {
	t.Helper()
	x, err := Open(filepath.Join(t.TempDir(), "nested", "index"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { x.Close() })
	if err := x.Apply(fixtures, nil, "1"); err != nil {
		t.Fatal(err)
	}
	return x
}

func ids(r *Result) string {
	var s []string
	for _, h := range r.Results {
		s = append(s, h.ID)
	}
	return strings.Join(s, ",")
}

func TestSearch(t *testing.T) {
	x := open(t)
	cases := []struct {
		name string
		q    Query
		want string
	}{
		{"and across fields", Query{Q: "wisata rahma"}, "repo:1/1"},
		{"title boost ranks first", Query{Q: "wisata"}, "repo:1/1,repo:1/3"},
		{"contributor", Query{Q: "santoso"}, "repo:1/2"},
		{"stop words ignored", Query{Q: "wisata dan yang the"}, "repo:1/1,repo:1/3"},
		{"fuzzy one edit", Query{Q: "jeramy"}, "repo:1/2"},
		{"affix variant", Query{Q: "memasarkan"}, "repo:1/3"},
		{"phrase", Query{Q: `"wisata halal"`}, "repo:1/3"},
		{"phrase in subject", Query{Q: `"pakan ternak"`}, "repo:1/2"},
		{"phrase in creator", Query{Q: `"rahma siti"`}, "repo:1/1"},
		{"or fallback", Query{Q: "ziarah ternak"}, "repo:1/1,repo:1/2"},
		{"type filter", Query{Q: "wisata", Types: []string{"Article"}}, "repo:1/3"},
		{"collection filter", Query{Q: "pakan wisata", Collections: []string{"Fakultas Peternakan"}}, "repo:1/2"},
		{"year range", Query{Q: "wisata", YearFrom: "2021", YearTo: "2024"}, "repo:1/3"},
		{"newest", Query{Q: "wisata", Newest: true}, "repo:1/3,repo:1/1"},
		{"only stop words", Query{Q: "yang dan"}, ""},
		{"no match", Query{Q: "kuantum"}, ""},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			c.q.Page, c.q.Limit = 1, 10
			r, err := x.Search(c.q)
			if err != nil {
				t.Fatal(err)
			}
			if got := ids(r); !strings.HasPrefix(got, c.want) || (c.want == "" && got != "") {
				t.Errorf("got %q, want prefix %q", got, c.want)
			}
		})
	}
}

func TestSearchMostWords(t *testing.T) {
	x := open(t)
	if err := x.Apply([]record.Record{
		{ID: "slims:1", Source: "slims", Title: "Hukum acara pidana"},
		{ID: "repo:2/1", Source: "repo", Title: "Pemalsuan data wali nikah", Abstract: []string{"Penulis mencari buku hukum acara pidana di perpustakaan."}},
	}, nil, "1"); err != nil {
		t.Fatal(err)
	}
	cases := []struct{ q, in, out string }{
		{"cari buku hukum acara pidana", "slims:1", ""},
		{"buku hukum pidana", "repo:2/1", "slims:1"},
		{`"wali nikah" cari buku hukum acara`, "repo:2/1", "slims:1"},
	}
	for _, c := range cases {
		r, err := x.Search(Query{Q: c.q, Page: 1, Limit: 10})
		if err != nil {
			t.Fatal(err)
		}
		got := ids(r)
		if !strings.Contains(got, c.in) || (c.out != "" && strings.Contains(got, c.out)) {
			t.Errorf("%q: got %q, want %s and not %q", c.q, got, c.in, c.out)
		}
	}
}

func TestTitlePhrase(t *testing.T) {
	x := open(t)
	if err := x.Apply([]record.Record{
		{ID: "slims:10", Source: "slims", Title: "Bank dan lembaga keuangan lain: teori, praktik, regulasi, dan perkembangannya di Indonesia"},
		{ID: "slims:11", Source: "slims", Title: "Lembaga keuangan bank"},
		{ID: "slims:12", Source: "slims", Title: "Bank lembaga perkreditan rakyat keuangan"},
	}, nil, "1"); err != nil {
		t.Fatal(err)
	}
	cases := []struct{ q, want string }{
		{"bank lembaga keuangan", "slims:10,"},
		{`"bank lembaga"`, "slims:12"},
	}
	for _, c := range cases {
		r, err := x.Search(Query{Q: c.q, Page: 1, Limit: 10})
		if err != nil {
			t.Fatal(err)
		}
		if got := ids(r) + ","; !strings.HasPrefix(got, c.want) || (!strings.HasSuffix(c.want, ",") && got != c.want+",") {
			t.Errorf("%q: got %q, want %q", c.q, ids(r), c.want)
		}
	}
}

func TestScoresIgnoreSegments(t *testing.T) {
	scores := func(batches [][]record.Record) []float64 {
		x, err := Open(filepath.Join(t.TempDir(), "index"))
		if err != nil {
			t.Fatal(err)
		}
		defer x.Close()
		for _, b := range batches {
			if err := x.Apply(b, nil, "1"); err != nil {
				t.Fatal(err)
			}
		}
		r, err := x.Search(Query{Q: "wisata", Page: 1, Limit: 10})
		if err != nil {
			t.Fatal(err)
		}
		var s []float64
		for _, h := range r.Results {
			s = append(s, h.Score)
		}
		return s
	}
	one := scores([][]record.Record{fixtures})
	many := scores([][]record.Record{fixtures[:1], fixtures[1:2], fixtures[2:]})
	if len(one) == 0 || fmt.Sprint(one) != fmt.Sprint(many) {
		t.Fatalf("one batch %v, three batches %v", one, many)
	}
}

func TestTiesSortByID(t *testing.T) {
	for _, order := range [][]string{{"slims:9", "slims:8"}, {"slims:8", "slims:9"}} {
		x := open(t)
		for _, id := range order {
			if err := x.Apply([]record.Record{{ID: id, Source: "slims", Title: "Hukum lingkungan"}}, nil, "1"); err != nil {
				t.Fatal(err)
			}
		}
		for _, newest := range []bool{false, true} {
			r, err := x.Search(Query{Q: "hukum lingkungan", Newest: newest, Page: 1, Limit: 10})
			if err != nil {
				t.Fatal(err)
			}
			if got := ids(r); got != "slims:8,slims:9" {
				t.Errorf("indexed %v, newest %t: got %q", order, newest, got)
			}
		}
	}
}

func TestAnalyzers(t *testing.T) {
	path := filepath.Join(t.TempDir(), "index")
	x, err := Open(path)
	if err != nil {
		t.Fatal(err)
	}
	x.Close()
	if x, err = Open(path); err != nil {
		t.Fatal(err)
	}
	defer x.Close()
	for name, want := range map[string]string{analyzer: "konsep@1 diri@3 islam@5", phraseAnalyzer: "konsep@1 diri@2 islam@3"} {
		var got []string
		for _, tok := range x.b.Mapping().AnalyzerNamed(name).Analyze([]byte("konsep dan diri of islam")) {
			got = append(got, fmt.Sprintf("%s@%d", tok.Term, tok.Position))
		}
		if s := strings.Join(got, " "); s != want {
			t.Errorf("%s after reopen: %s, want %s", name, s, want)
		}
	}
}

func TestSearchResultShape(t *testing.T) {
	x := open(t)
	r, err := x.Search(Query{Q: "wisata", Page: 1, Limit: 10})
	if err != nil {
		t.Fatal(err)
	}
	h := r.Results[0]
	if h.Score <= 0 || h.Creator[0] != "Rahma, Siti" {
		t.Errorf("hit %+v", h)
	}
	if got := strings.Join(h.Highlight["title"], ""); !strings.Contains(got, "<mark>wisata</mark>") {
		t.Errorf("title highlight %q", got)
	}
	if f := r.Facets["isPartOf"]; len(f) == 0 || f[0] != (Facet{"Fakultas Ekonomi", 2}) {
		t.Errorf("isPartOf facet %v", f)
	}
	if f := r.Facets["year"]; len(f) != 2 {
		t.Errorf("year facet %v", f)
	}
}

func TestPaging(t *testing.T) {
	x := open(t)
	r, err := x.Search(Query{Q: "wisata", Page: 2, Limit: 1})
	if err != nil {
		t.Fatal(err)
	}
	if r.Total != 2 || ids(r) != "repo:1/3" {
		t.Fatalf("total %d, ids %q", r.Total, ids(r))
	}
}

func TestGetCountSweepMeta(t *testing.T) {
	x := open(t)
	r, err := x.Get("repo:1/2")
	if err != nil || r == nil || r.Contributor[0].Role != "advisor" {
		t.Fatalf("get %+v, %v", r, err)
	}
	if r, err := x.Get("repo:missing"); r != nil || err != nil {
		t.Fatalf("missing get %+v, %v", r, err)
	}
	if err := x.Apply(fixtures[:1], []string{"repo:1/3"}, "2"); err != nil {
		t.Fatal(err)
	}
	if n, _ := x.Count("repo"); n != 2 {
		t.Fatalf("count %d after delete", n)
	}
	if n, err := x.Sweep("repo", "2"); n != 1 || err != nil {
		t.Fatalf("swept %d, %v", n, err)
	}
	if r, _ := x.Get("repo:1/1"); r == nil {
		t.Fatal("current generation was swept")
	}
	if err := x.SetMeta("cursor:repo", "2026-10-26T05:00:00Z"); err != nil {
		t.Fatal(err)
	}
	if v, _ := x.Meta("cursor:repo"); v != "2026-10-26T05:00:00Z" {
		t.Fatalf("meta %q", v)
	}
}
