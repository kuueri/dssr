package index

import (
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
		{"phrase", Query{Q: `"wisata halal"`}, "repo:1/3"},
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
