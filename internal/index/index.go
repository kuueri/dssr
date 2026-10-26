package index

import (
	"encoding/json"
	"errors"

	"github.com/blevesearch/bleve/v2"
	"github.com/blevesearch/bleve/v2/analysis/analyzer/custom"
	"github.com/blevesearch/bleve/v2/analysis/lang/en"
	"github.com/blevesearch/bleve/v2/analysis/lang/id"
	"github.com/blevesearch/bleve/v2/analysis/token/lowercase"
	"github.com/blevesearch/bleve/v2/analysis/token/unicodenorm"
	"github.com/blevesearch/bleve/v2/analysis/tokenizer/unicode"
	"github.com/blevesearch/bleve/v2/mapping"
	"github.com/blevesearch/bleve/v2/search/query"
	bleveindex "github.com/blevesearch/bleve_index_api"

	"github.com/kuueri/dssr/internal/record"
)

const analyzer = "dssr"

type Index struct {
	b bleve.Index
}

func Open(path string) (*Index, error) {
	b, err := bleve.OpenUsing(path, map[string]interface{}{"bolt_timeout": "1s"})
	if errors.Is(err, bleve.ErrorIndexPathDoesNotExist) {
		var m *mapping.IndexMappingImpl
		if m, err = newMapping(); err == nil {
			b, err = bleve.New(path, m)
		}
	}
	if err != nil {
		return nil, err
	}
	return &Index{b: b}, nil
}

func (x *Index) Close() error { return x.b.Close() }

func newMapping() (*mapping.IndexMappingImpl, error) {
	m := bleve.NewIndexMapping()
	m.ScoringModel = bleveindex.BM25Scoring
	if err := m.AddCustomTokenFilter("nfkc", map[string]interface{}{
		"type": unicodenorm.Name,
		"form": unicodenorm.NFKC,
	}); err != nil {
		return nil, err
	}
	if err := m.AddCustomAnalyzer(analyzer, map[string]interface{}{
		"type":          custom.Name,
		"tokenizer":     unicode.Name,
		"token_filters": []string{"nfkc", lowercase.Name, en.StopName, id.StopName},
	}); err != nil {
		return nil, err
	}

	text := func(store bool) *mapping.FieldMapping {
		f := bleve.NewTextFieldMapping()
		f.Analyzer = analyzer
		f.Store = store
		f.IncludeTermVectors = store
		f.IncludeInAll = false
		f.DocValues = false
		return f
	}
	keyword := func(name string) *mapping.FieldMapping {
		f := bleve.NewKeywordFieldMapping()
		f.Name = name
		f.Store = false
		f.IncludeInAll = false
		return f
	}

	d := bleve.NewDocumentStaticMapping()
	d.AddFieldMappingsAt("title", text(true))
	d.AddFieldMappingsAt("abstract", text(true))
	d.AddFieldMappingsAt("creator", text(false))
	d.AddFieldMappingsAt("contributor", text(false))
	d.AddFieldMappingsAt("subject", text(false), keyword("subject_facet"))
	for _, f := range []string{"source", "gen", "type", "language", "isPartOf", "year", "issued"} {
		d.AddFieldMappingsAt(f, keyword(""))
	}
	raw := bleve.NewTextFieldMapping()
	raw.Index = false
	raw.Store = true
	raw.IncludeInAll = false
	raw.DocValues = false
	d.AddFieldMappingsAt("record", raw)

	m.DefaultMapping = d
	m.IndexDynamic = false
	m.StoreDynamic = false
	m.DocValuesDynamic = false
	return m, nil
}

func toDoc(r record.Record, gen string) (map[string]interface{}, error) {
	js, err := json.Marshal(r)
	if err != nil {
		return nil, err
	}
	d := map[string]interface{}{"source": r.Source, "gen": gen, "record": string(js)}
	set := func(k string, v []string) {
		if len(v) > 0 {
			d[k] = v
		}
	}
	names := make([]string, len(r.Contributor))
	for i, c := range r.Contributor {
		names[i] = c.Name
	}
	set("title", nonEmpty(r.Title))
	set("abstract", r.Abstract)
	set("creator", r.Creator)
	set("contributor", names)
	set("subject", r.Subject)
	set("type", r.Type)
	set("language", r.Language)
	set("isPartOf", r.IsPartOf)
	set("year", nonEmpty(r.Year()))
	set("issued", nonEmpty(r.Issued))
	return d, nil
}

func nonEmpty(s string) []string {
	if s == "" {
		return nil
	}
	return []string{s}
}

func (x *Index) Apply(upserts []record.Record, deletes []string, gen string) error {
	b := x.b.NewBatch()
	for _, r := range upserts {
		d, err := toDoc(r, gen)
		if err != nil {
			return err
		}
		if err := b.Index(r.ID, d); err != nil {
			return err
		}
	}
	for _, id := range deletes {
		b.Delete(id)
	}
	return x.b.Batch(b)
}

func (x *Index) Get(id string) (*record.Record, error) {
	d, err := x.b.Document(id)
	if err != nil || d == nil {
		return nil, err
	}
	var raw []byte
	d.VisitFields(func(f bleveindex.Field) {
		if f.Name() == "record" {
			raw = f.Value()
		}
	})
	if raw == nil {
		return nil, nil
	}
	var r record.Record
	if err := json.Unmarshal(raw, &r); err != nil {
		return nil, err
	}
	return &r, nil
}

func (x *Index) Meta(key string) (string, error) {
	v, err := x.b.GetInternal([]byte(key))
	return string(v), err
}

func (x *Index) SetMeta(key, val string) error {
	return x.b.SetInternal([]byte(key), []byte(val))
}

func (x *Index) Count(source string) (uint64, error) {
	res, err := x.b.Search(bleve.NewSearchRequestOptions(term("source", source), 0, 0, false))
	if err != nil {
		return 0, err
	}
	return res.Total, nil
}

func (x *Index) Sweep(source, gen string) (int, error) {
	q := query.NewBooleanQuery([]query.Query{term("source", source)}, nil, []query.Query{term("gen", gen)})
	n := 0
	for {
		res, err := x.b.Search(bleve.NewSearchRequestOptions(q, 1000, 0, false))
		if err != nil || len(res.Hits) == 0 {
			return n, err
		}
		b := x.b.NewBatch()
		for _, h := range res.Hits {
			b.Delete(h.ID)
		}
		if err := x.b.Batch(b); err != nil {
			return n, err
		}
		n += len(res.Hits)
	}
}

func term(field, value string) *query.TermQuery {
	q := query.NewTermQuery(value)
	q.SetField(field)
	return q
}
