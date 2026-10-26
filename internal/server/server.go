package server

import (
	"crypto/sha256"
	"crypto/subtle"
	_ "embed"
	"encoding/hex"
	"encoding/json"
	"errors"
	"log/slog"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/kuueri/dssr/internal/config"
	"github.com/kuueri/dssr/internal/harvest"
	"github.com/kuueri/dssr/internal/index"
)

//go:embed openapi.json
var openapi []byte

type Server struct {
	idx     *index.Index
	h       *harvest.Harvester
	sources []string
	tokens  [][]byte
	version string
	started time.Time
}

type Status struct {
	Version string                 `json:"version"`
	Started time.Time              `json:"started"`
	Sources []harvest.SourceStatus `json:"sources"`
}

func New(idx *index.Index, h *harvest.Harvester, cfg *config.Config, version string) *Server {
	s := &Server{idx: idx, h: h, version: version, started: time.Now()}
	for _, c := range cfg.Clients {
		b, _ := hex.DecodeString(c.TokenSHA256)
		s.tokens = append(s.tokens, b)
	}
	for _, src := range cfg.Sources {
		s.sources = append(s.sources, src.ID)
	}
	return s
}

func (s *Server) API() http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /healthz", func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte("ok\n"))
	})
	mux.HandleFunc("GET /openapi.json", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.Write(openapi)
	})
	mux.Handle("GET /v1/search", s.auth(s.search))
	mux.Handle("GET /v1/records/{source}/{handle...}", s.auth(s.record))
	return mux
}

func (s *Server) Admin() http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /status", s.status)
	mux.HandleFunc("POST /harvest", s.harvest)
	mux.HandleFunc("GET /search", s.search)
	mux.HandleFunc("GET /records/{source}/{handle...}", s.record)
	return mux
}

func (s *Server) auth(next http.HandlerFunc) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		tok, ok := strings.CutPrefix(r.Header.Get("Authorization"), "Bearer ")
		sum := sha256.Sum256([]byte(tok))
		match := 0
		for _, t := range s.tokens {
			match |= subtle.ConstantTimeCompare(sum[:], t)
		}
		if !ok || tok == "" || match == 0 {
			w.Header().Set("WWW-Authenticate", "Bearer")
			problem(w, http.StatusUnauthorized, "missing or invalid bearer token")
			return
		}
		next(w, r)
	})
}

func (s *Server) search(w http.ResponseWriter, r *http.Request) {
	q, err := ParseQuery(r.URL.Query())
	if err != nil {
		problem(w, http.StatusBadRequest, err.Error())
		return
	}
	res, err := s.idx.Search(q)
	if err != nil {
		slog.Error("search failed", "q", q.Q, "err", err)
		problem(w, http.StatusInternalServerError, "search failed")
		return
	}
	writeJSON(w, http.StatusOK, res)
}

func (s *Server) record(w http.ResponseWriter, r *http.Request) {
	rec, err := s.idx.Get(r.PathValue("source") + ":" + r.PathValue("handle"))
	if err != nil {
		slog.Error("get failed", "err", err)
		problem(w, http.StatusInternalServerError, "lookup failed")
		return
	}
	if rec == nil {
		problem(w, http.StatusNotFound, "record not found")
		return
	}
	writeJSON(w, http.StatusOK, rec)
}

func (s *Server) status(w http.ResponseWriter, r *http.Request) {
	sources, err := s.h.Status()
	if err != nil {
		problem(w, http.StatusInternalServerError, err.Error())
		return
	}
	writeJSON(w, http.StatusOK, Status{Version: s.version, Started: s.started, Sources: sources})
}

func (s *Server) harvest(w http.ResponseWriter, r *http.Request) {
	ids := s.sources
	if id := r.URL.Query().Get("source"); id != "" {
		ids = []string{id}
	}
	full := r.URL.Query().Get("full") == "true"
	for _, id := range ids {
		switch err := s.h.Start(id, full); {
		case errors.Is(err, harvest.ErrUnknownSource):
			problem(w, http.StatusNotFound, "unknown source "+id)
			return
		case errors.Is(err, harvest.ErrBusy):
			problem(w, http.StatusConflict, "a harvest is already running for "+id)
			return
		}
	}
	writeJSON(w, http.StatusAccepted, map[string]any{"started": ids, "full": full})
}

func ParseQuery(v url.Values) (index.Query, error) {
	q := index.Query{
		Q:           strings.TrimSpace(v.Get("q")),
		Types:       v["type"],
		Collections: v["collection"],
		Languages:   v["language"],
	}
	if q.Q == "" {
		return q, errors.New("q is required")
	}
	if utf8.RuneCountInString(q.Q) > 200 {
		return q, errors.New("q is longer than 200 characters")
	}
	if y := v.Get("year"); y != "" {
		from, to, found := strings.Cut(y, "-")
		if !found {
			to = from
		}
		if !isYear(from) || !isYear(to) || from > to {
			return q, errors.New("year must be YYYY or YYYY-YYYY")
		}
		q.YearFrom, q.YearTo = from, to
	}
	switch v.Get("sort") {
	case "", "relevance":
	case "newest":
		q.Newest = true
	default:
		return q, errors.New("sort must be relevance or newest")
	}
	var err error
	if q.Page, err = intParam(v, "page", 1); err != nil || q.Page < 1 {
		return q, errors.New("page must be a positive integer")
	}
	if q.Limit, err = intParam(v, "limit", 10); err != nil || q.Limit < 1 || q.Limit > 50 {
		return q, errors.New("limit must be between 1 and 50")
	}
	if q.Page > 1000/q.Limit {
		return q, errors.New("results beyond position 1000 are not served")
	}
	return q, nil
}

func intParam(v url.Values, key string, def int) (int, error) {
	if s := v.Get(key); s != "" {
		return strconv.Atoi(s)
	}
	return def, nil
}

func isYear(s string) bool {
	if len(s) != 4 {
		return false
	}
	for _, c := range s {
		if c < '0' || c > '9' {
			return false
		}
	}
	return true
}

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	json.NewEncoder(w).Encode(v)
}

type Problem struct {
	Type   string `json:"type"`
	Title  string `json:"title"`
	Status int    `json:"status"`
	Detail string `json:"detail"`
}

func problem(w http.ResponseWriter, status int, detail string) {
	w.Header().Set("Content-Type", "application/problem+json")
	w.WriteHeader(status)
	json.NewEncoder(w).Encode(Problem{Type: "about:blank", Title: http.StatusText(status), Status: status, Detail: detail})
}
