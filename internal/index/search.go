package index

import (
	"context"
	"encoding/json"
	"slices"
	"strings"
	"unicode/utf8"

	"github.com/blevesearch/bleve/v2"
	"github.com/blevesearch/bleve/v2/search"
	"github.com/blevesearch/bleve/v2/search/highlight/highlighter/html"
	"github.com/blevesearch/bleve/v2/search/query"

	"github.com/kuueri/dssr/internal/record"
)

type Query struct {
	Q           string
	Types       []string
	YearFrom    string
	YearTo      string
	Collections []string
	Languages   []string
	Newest      bool
	Page        int
	Limit       int
}

type Hit struct {
	record.Record
	Highlight map[string][]string `json:"highlight,omitempty"`
	Score     float64             `json:"score"`
}

type Facet struct {
	Value string `json:"value"`
	Count int    `json:"count"`
}

type Result struct {
	Total   uint64             `json:"total"`
	Page    int                `json:"page"`
	Limit   int                `json:"limit"`
	Results []Hit              `json:"results"`
	Facets  map[string][]Facet `json:"facets"`
}

var boosts = []struct {
	field string
	boost float64
}{{"title", 3}, {"creator", 2}, {"contributor", 2}, {"subject", 2}, {"abstract", 1}}

const fuzzyWeight = 0.2

var facetFields = map[string]string{
	"type":     "type",
	"year":     "year",
	"isPartOf": "isPartOf",
	"language": "language",
	"subject":  "subject_facet",
}

func (x *Index) Search(q Query) (*Result, error) {
	out := &Result{Page: q.Page, Limit: q.Limit, Results: []Hit{}, Facets: map[string][]Facet{}}
	words, phrases := split(q.Q)
	a := x.b.Mapping().AnalyzerNamed(analyzer)

	var clauses []query.Query
	tokens := a.Analyze([]byte(strings.Join(words, " ")))
	for _, t := range tokens {
		clauses = append(clauses, termClause(string(t.Term)))
	}
	for _, p := range phrases {
		if len(a.Analyze([]byte(p))) > 0 {
			clauses = append(clauses, phraseClause(p))
		}
	}
	if len(clauses) == 0 {
		return out, nil
	}
	var should []query.Query
	if len(tokens) > 1 {
		p := query.NewMatchPhraseQuery(strings.Join(words, " "))
		p.SetField("title_phrase")
		p.Analyzer = phraseAnalyzer
		p.SetBoost(3)
		should = append(should, p)
	}
	filter := filters(q)

	must := clauses
	if n := len(tokens); n >= 4 {
		terms := query.NewDisjunctionQuery(clauses[:n])
		terms.SetMin(float64(n * 3 / 4))
		must = append([]query.Query{terms}, clauses[n:]...)
	}
	bq := query.NewBooleanQuery(must, should, nil)
	bq.AddFilter(filter)
	ctx := context.WithValue(context.Background(), search.BM25StatsKey, x.stats.Load())
	res, err := x.b.SearchInContext(ctx, x.request(bq, q))
	if err == nil && res.Total == 0 && len(clauses) > 1 {
		bq = query.NewBooleanQuery(nil, append(slices.Clone(clauses), should...), nil)
		bq.SetMinShould(1)
		bq.AddFilter(filter)
		res, err = x.b.SearchInContext(ctx, x.request(bq, q))
	}
	if err != nil {
		return nil, err
	}

	out.Total = res.Total
	for _, h := range res.Hits {
		raw, _ := h.Fields["record"].(string)
		hit := Hit{Score: h.Score}
		if err := json.Unmarshal([]byte(raw), &hit.Record); err != nil {
			return nil, err
		}
		if len(h.Fragments) > 0 {
			hit.Highlight = h.Fragments
		}
		out.Results = append(out.Results, hit)
	}
	for name, f := range res.Facets {
		terms := f.Terms.Terms()
		fs := make([]Facet, len(terms))
		for i, t := range terms {
			fs[i] = Facet{Value: t.Term, Count: t.Count}
		}
		out.Facets[name] = fs
	}
	return out, nil
}

func (x *Index) request(q query.Query, p Query) *bleve.SearchRequest {
	req := bleve.NewSearchRequestOptions(q, p.Limit, (p.Page-1)*p.Limit, false)
	req.Fields = []string{"record"}
	req.Highlight = bleve.NewHighlightWithStyle(html.Name)
	req.Highlight.AddField("title")
	req.Highlight.AddField("abstract")
	for name, field := range facetFields {
		req.AddFacet(name, bleve.NewFacetRequest(field, 10))
	}
	order := search.SortOrder{&search.SortScore{Desc: true}, &search.SortDocID{}}
	if p.Newest {
		order = append(search.SortOrder{&search.SortField{Field: "issued", Desc: true, Type: search.SortFieldAsString, Missing: search.SortFieldMissingLast}}, order...)
	}
	req.SortByCustom(order)
	return req
}

func split(s string) (words, phrases []string) {
	for i, part := range strings.Split(s, `"`) {
		if i%2 == 1 {
			phrases = append(phrases, part)
		} else {
			words = append(words, part)
		}
	}
	return words, phrases
}

func termClause(tok string) query.Query {
	var qs []query.Query
	for _, b := range boosts {
		t := term(b.field, tok)
		t.SetBoost(b.boost)
		qs = append(qs, t)
		if utf8.RuneCountInString(tok) >= 5 {
			f := query.NewFuzzyQuery(tok)
			f.SetField(b.field)
			f.SetFuzziness(1)
			f.SetBoost(b.boost * fuzzyWeight)
			qs = append(qs, f)
		}
	}
	return query.NewDisjunctionQuery(qs)
}

func phraseClause(p string) query.Query {
	var qs []query.Query
	for _, b := range boosts {
		m := query.NewMatchPhraseQuery(p)
		m.SetField(b.field)
		m.SetBoost(b.boost)
		qs = append(qs, m)
	}
	return query.NewDisjunctionQuery(qs)
}

func filters(q Query) query.Query {
	var fs []query.Query
	anyOf := func(field string, values []string) {
		if len(values) == 0 {
			return
		}
		qs := make([]query.Query, len(values))
		for i, v := range values {
			qs[i] = term(field, v)
		}
		fs = append(fs, query.NewDisjunctionQuery(qs))
	}
	anyOf("type", q.Types)
	anyOf("isPartOf", q.Collections)
	anyOf("language", q.Languages)
	if q.YearFrom != "" {
		in := true
		r := query.NewTermRangeInclusiveQuery(q.YearFrom, q.YearTo, &in, &in)
		r.SetField("year")
		fs = append(fs, r)
	}
	if len(fs) == 0 {
		return nil
	}
	return query.NewConjunctionQuery(fs)
}
