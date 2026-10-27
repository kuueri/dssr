package index

import (
	"strings"

	"github.com/blevesearch/bleve/v2/analysis"
	"github.com/blevesearch/bleve/v2/registry"
)

const stemmer = "dssr_stem_id"

func init() {
	err := registry.RegisterTokenFilter(stemmer, func(map[string]any, *registry.Cache) (analysis.TokenFilter, error) {
		return stemFilter{}, nil
	})
	if err != nil {
		panic(err)
	}
}

type stemFilter struct{}

func (stemFilter) Filter(input analysis.TokenStream) analysis.TokenStream {
	for _, t := range input {
		t.Term = []byte(stem(string(t.Term)))
	}
	return input
}

const (
	removedKe = 1 << iota
	removedPeng
	removedDi
	removedMeng
	removedTer
	removedBer
	removedPe
)

type stemState struct {
	s         string
	syllables int
	flags     int
}

func stem(s string) string {
	w := &stemState{s: s}
	for i := 0; i < len(s); i++ {
		if isVowel(s[i]) {
			w.syllables++
		}
	}
	w.particle()
	w.possessive()
	n := len(w.s)
	w.firstOrderPrefix()
	if len(w.s) == n {
		w.secondOrderPrefix()
		w.suffix()
		return w.s
	}
	n = len(w.s)
	w.suffix()
	if len(w.s) != n {
		w.secondOrderPrefix()
	}
	return w.s
}

func isVowel(c byte) bool {
	return strings.IndexByte("aeiou", c) >= 0
}

func (w *stemState) cut(n int, repl string, flag int) {
	w.s = repl + w.s[n:]
	w.syllables--
	w.flags |= flag
}

func (w *stemState) trim(n int) {
	w.s = w.s[:len(w.s)-n]
	w.syllables--
}

func (w *stemState) particle() {
	if w.syllables > 2 && (strings.HasSuffix(w.s, "kah") || strings.HasSuffix(w.s, "lah") || strings.HasSuffix(w.s, "pun")) {
		w.trim(3)
	}
}

func (w *stemState) possessive() {
	if w.syllables <= 2 {
		return
	}
	switch {
	case strings.HasSuffix(w.s, "ku"), strings.HasSuffix(w.s, "mu"):
		w.trim(2)
	case strings.HasSuffix(w.s, "nya"):
		w.trim(3)
	}
}

func (w *stemState) firstOrderPrefix() {
	if w.syllables <= 2 {
		return
	}
	s := w.s
	switch {
	case strings.HasPrefix(s, "meng"):
		w.cut(4, "", removedMeng)
	case strings.HasPrefix(s, "meny") && len(s) > 4 && isVowel(s[4]):
		w.cut(4, "s", removedMeng)
	case strings.HasPrefix(s, "men"), strings.HasPrefix(s, "mem"):
		w.cut(3, "", removedMeng)
	case strings.HasPrefix(s, "me"):
		w.cut(2, "", removedMeng)
	case strings.HasPrefix(s, "peng"):
		w.cut(4, "", removedPeng)
	case strings.HasPrefix(s, "peny") && len(s) > 4 && isVowel(s[4]):
		w.cut(4, "s", removedPeng)
	case strings.HasPrefix(s, "peny"):
		w.cut(4, "", removedPeng)
	case strings.HasPrefix(s, "pen") && len(s) > 3 && isVowel(s[3]):
		w.cut(3, "t", removedPeng)
	case strings.HasPrefix(s, "pen"), strings.HasPrefix(s, "pem"):
		w.cut(3, "", removedPeng)
	case strings.HasPrefix(s, "di"):
		w.cut(2, "", removedDi)
	case strings.HasPrefix(s, "ter"):
		w.cut(3, "", removedTer)
	case strings.HasPrefix(s, "ke"):
		w.cut(2, "", removedKe)
	}
}

func (w *stemState) secondOrderPrefix() {
	if w.syllables <= 2 {
		return
	}
	s := w.s
	switch {
	case strings.HasPrefix(s, "ber"), s == "belajar":
		w.cut(3, "", removedBer)
	case len(s) > 4 && strings.HasPrefix(s, "be") && !isVowel(s[2]) && s[3:5] == "er":
		w.cut(2, "", removedBer)
	case strings.HasPrefix(s, "per"), s == "pelajar":
		w.cut(3, "", 0)
	case strings.HasPrefix(s, "pe"):
		w.cut(2, "", removedPe)
	}
}

func (w *stemState) suffix() {
	if w.syllables <= 2 {
		return
	}
	s := w.s
	switch {
	case strings.HasSuffix(s, "kan") && w.flags&(removedKe|removedPeng|removedPe) == 0:
		w.trim(3)
	case strings.HasSuffix(s, "an") && w.flags&(removedDi|removedMeng|removedTer) == 0:
		w.trim(2)
	case strings.HasSuffix(s, "i") && !strings.HasSuffix(s, "si") && w.flags&(removedBer|removedKe|removedPeng) == 0:
		w.trim(1)
	}
}
