package index

import (
	"cmp"
	"encoding/json"
	"errors"
	"slices"
	"sync"
	"sync/atomic"

	"github.com/blevesearch/bleve/v2"
	"github.com/blevesearch/bleve/v2/analysis"
	"github.com/blevesearch/bleve/v2/analysis/analyzer/custom"
	"github.com/blevesearch/bleve/v2/analysis/lang/en"
	"github.com/blevesearch/bleve/v2/analysis/lang/id"
	"github.com/blevesearch/bleve/v2/analysis/token/lowercase"
	"github.com/blevesearch/bleve/v2/analysis/token/stop"
	"github.com/blevesearch/bleve/v2/analysis/token/unicodenorm"
	"github.com/blevesearch/bleve/v2/analysis/tokenizer/unicode"
	"github.com/blevesearch/bleve/v2/analysis/tokenmap"
	"github.com/blevesearch/bleve/v2/mapping"
	"github.com/blevesearch/bleve/v2/registry"
	"github.com/blevesearch/bleve/v2/search"
	"github.com/blevesearch/bleve/v2/search/query"
	bleveindex "github.com/blevesearch/bleve_index_api"

	"github.com/kuueri/dssr/internal/record"
)

const (
	analyzer       = "dssr"
	phraseAnalyzer = "dssr_phrase"
)

var contentWords = []string{
	"banyak", "bermacam", "bersama", "biasa", "bukan", "dahulu", "dekat", "depan", "dini", "diri", "dirinya",
	"dulu", "hal", "ibarat", "kecil", "kini", "lain", "lainnya", "lalu", "lama", "lamanya", "macam", "mampu",
	"nanti", "pantas", "percuma", "saat", "saatnya", "sama", "sedikit", "sekarang", "sekitar", "sela",
	"sendiri", "sendirinya", "serupa", "sesama", "tadi", "tak", "tanpa", "tertentu", "tidak",
}

const gapless = "dssr_gapless"

func init() {
	err := registry.RegisterTokenFilter(gapless, func(map[string]any, *registry.Cache) (analysis.TokenFilter, error) {
		return gaplessFilter{}, nil
	})
	if err != nil {
		panic(err)
	}
}

type gaplessFilter struct{}

func (gaplessFilter) Filter(input analysis.TokenStream) analysis.TokenStream {
	for i, t := range input {
		t.Position = i + 1
	}
	return input
}

type Index struct {
	b     bleve.Index
	mu    sync.Mutex
	stats atomic.Pointer[search.BM25Stats]
}

func Open(path string) (*Index, error) {
	b, err := bleve.OpenUsing(path, map[string]any{"bolt_timeout": "1s"})
	if errors.Is(err, bleve.ErrorIndexPathDoesNotExist) {
		var m *mapping.IndexMappingImpl
		if m, err = newMapping(); err == nil {
			b, err = bleve.New(path, m)
		}
	}
	if err != nil {
		return nil, err
	}
	x := &Index{b: b}
	if err := x.refreshStats(); err != nil {
		b.Close()
		return nil, err
	}
	return x, nil
}

func (x *Index) refreshStats() error {
	x.mu.Lock()
	defer x.mu.Unlock()
	adv, err := x.b.Advanced()
	if err != nil {
		return err
	}
	r, err := adv.Reader()
	if err != nil {
		return err
	}
	defer r.Close()
	n, err := r.DocCount()
	if err != nil {
		return err
	}
	st := &search.BM25Stats{DocCount: float64(n), FieldCardinality: map[string]int{"*": 0}}
	for path, dm := range x.b.Mapping().(*mapping.IndexMappingImpl).DefaultMapping.Properties {
		for _, f := range dm.Fields {
			st.FieldCardinality[cmp.Or(f.Name, path)] = 0
		}
	}
	fields, err := r.Fields()
	if err != nil {
		return err
	}
	for _, f := range fields {
		d, err := r.FieldDict(f)
		if err != nil {
			return err
		}
		total := 0
		for {
			e, err := d.Next()
			if err != nil {
				d.Close()
				return err
			}
			if e == nil {
				break
			}
			total += int(e.Count)
		}
		d.Close()
		st.FieldCardinality[f] = total
	}
	x.stats.Store(st)
	return nil
}

func (x *Index) Close() error { return x.b.Close() }

func newMapping() (*mapping.IndexMappingImpl, error) {
	m := bleve.NewIndexMapping()
	m.ScoringModel = bleveindex.BM25Scoring
	if err := m.AddCustomTokenFilter("nfkc", map[string]any{
		"type": unicodenorm.Name,
		"form": unicodenorm.NFKC,
	}); err != nil {
		return nil, err
	}
	words := analysis.NewTokenMap()
	if err := words.LoadBytes(id.IndonesianStopWords); err != nil {
		return nil, err
	}
	var tokens []any
	for w := range words {
		if !slices.Contains(contentWords, w) {
			tokens = append(tokens, w)
		}
	}
	if err := m.AddCustomTokenMap("id_function_words", map[string]any{"type": tokenmap.Name, "tokens": tokens}); err != nil {
		return nil, err
	}
	if err := m.AddCustomTokenFilter("stop_id_function", map[string]any{"type": stop.Name, "stop_token_map": "id_function_words"}); err != nil {
		return nil, err
	}
	filters := []string{"nfkc", lowercase.Name, en.StopName, "stop_id_function", stemmer}
	if err := m.AddCustomAnalyzer(analyzer, map[string]any{
		"type":          custom.Name,
		"tokenizer":     unicode.Name,
		"token_filters": filters,
	}); err != nil {
		return nil, err
	}
	if err := m.AddCustomAnalyzer(phraseAnalyzer, map[string]any{
		"type":          custom.Name,
		"tokenizer":     unicode.Name,
		"token_filters": append(filters, gapless),
	}); err != nil {
		return nil, err
	}

	text := func(store bool) *mapping.FieldMapping {
		f := bleve.NewTextFieldMapping()
		f.Analyzer = analyzer
		f.Store = store
		f.IncludeTermVectors = true
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

	phrase := text(false)
	phrase.Name = "title_phrase"
	phrase.Analyzer = phraseAnalyzer

	d := bleve.NewDocumentStaticMapping()
	d.AddFieldMappingsAt("title", text(true), phrase)
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

func toDoc(r record.Record, gen string) (map[string]any, error) {
	js, err := json.Marshal(r)
	if err != nil {
		return nil, err
	}
	d := map[string]any{"source": r.Source, "gen": gen, "record": string(js)}
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
	if err := x.b.Batch(b); err != nil {
		return err
	}
	return x.refreshStats()
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
		if err != nil {
			return n, err
		}
		if len(res.Hits) == 0 {
			return n, x.refreshStats()
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
