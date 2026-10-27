package harvest

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"maps"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/robfig/cron/v3"

	"github.com/kuueri/dssr/internal/config"
	"github.com/kuueri/dssr/internal/index"
	"github.com/kuueri/dssr/internal/oai"
	"github.com/kuueri/dssr/internal/record"
)

var (
	ErrBusy          = errors.New("a harvest is already running for this source")
	ErrUnknownSource = errors.New("unknown source")
)

type Run struct {
	Full     bool      `json:"full"`
	Started  time.Time `json:"started"`
	Finished time.Time `json:"finished"`
	Upserted int       `json:"upserted"`
	Deleted  int       `json:"deleted"`
	Swept    int       `json:"swept"`
	Note     string    `json:"note,omitempty"`
	Error    string    `json:"error,omitempty"`
}

type SourceStatus struct {
	ID      string         `json:"id"`
	Name    string         `json:"name"`
	Docs    uint64         `json:"docs"`
	Cursor  string         `json:"cursor,omitempty"`
	Next    *time.Time     `json:"next,omitempty"`
	Running bool           `json:"running"`
	Last    *Run           `json:"last,omitempty"`
	Dropped map[string]int `json:"dropped,omitempty"`
}

type state struct {
	running bool
	last    *Run
	dropped map[string]int
	entry   cron.EntryID
}

type Harvester struct {
	ctx     context.Context
	idx     *index.Index
	sources []config.Source
	agent   string
	cron    *cron.Cron
	wg      sync.WaitGroup
	mu      sync.Mutex
	state   map[string]*state
}

func New(ctx context.Context, idx *index.Index, sources []config.Source, userAgent string) *Harvester {
	h := &Harvester{ctx: ctx, idx: idx, sources: sources, agent: userAgent, state: map[string]*state{}}
	for _, s := range sources {
		h.state[s.ID] = &state{dropped: map[string]int{}}
	}
	return h
}

func (h *Harvester) Schedule(loc *time.Location) error {
	h.cron = cron.New(cron.WithLocation(loc))
	for _, s := range h.sources {
		id := s.ID
		e, err := h.cron.AddFunc(s.Schedule, func() {
			if err := h.Start(id, false); err != nil {
				slog.Warn("scheduled harvest not started", "source", id, "err", err)
			}
		})
		if err != nil {
			return fmt.Errorf("source %s: %w", id, err)
		}
		h.mu.Lock()
		h.state[id].entry = e
		h.mu.Unlock()
	}
	h.cron.Start()
	return nil
}

func (h *Harvester) Stop() {
	if h.cron != nil {
		<-h.cron.Stop().Done()
	}
	h.wg.Wait()
}

func (h *Harvester) Start(id string, full bool) error {
	if err := h.begin(id); err != nil {
		return err
	}
	h.wg.Go(func() { h.run(id, full) })
	return nil
}

func (h *Harvester) Run(id string, full bool) (Run, error) {
	if err := h.begin(id); err != nil {
		return Run{}, err
	}
	return h.run(id, full)
}

func (h *Harvester) begin(id string) error {
	h.mu.Lock()
	defer h.mu.Unlock()
	st, ok := h.state[id]
	if !ok {
		return ErrUnknownSource
	}
	if st.running {
		return ErrBusy
	}
	st.running = true
	return nil
}

func (h *Harvester) run(id string, full bool) (Run, error) {
	var src config.Source
	for _, s := range h.sources {
		if s.ID == id {
			src = s
		}
	}
	run := Run{Full: full, Started: time.Now()}
	slog.Info("harvest started", "source", id, "full", full)
	dropped := map[string]int{}
	err := h.harvest(src, full, &run, dropped)
	run.Finished = time.Now()
	if err != nil {
		run.Error = err.Error()
	}

	h.mu.Lock()
	st := h.state[id]
	st.running = false
	st.last = &run
	for k, n := range dropped {
		st.dropped[k] += n
	}
	h.mu.Unlock()

	attrs := []any{"source", id, "full", full, "upserted", run.Upserted, "deleted", run.Deleted, "swept", run.Swept, "seconds", int(run.Finished.Sub(run.Started).Seconds())}
	if err != nil {
		slog.Error("harvest failed", append(attrs, "err", err)...)
	} else {
		slog.Info("harvest finished", append(attrs, "note", run.Note)...)
	}
	return run, err
}

func (h *Harvester) harvest(src config.Source, full bool, run *Run, dropped map[string]int) error {
	c := oai.New(src.OAIURL, h.agent)
	sets, err := c.ListSets(h.ctx)
	if err != nil {
		return fmt.Errorf("ListSets: %w", err)
	}
	before, err := h.idx.Count(src.ID)
	if err != nil {
		return err
	}
	gen, err := h.idx.Meta("gen:" + src.ID)
	if err != nil {
		return err
	}
	if gen == "" {
		gen = "0"
	}
	from := ""
	if full {
		n, _ := strconv.Atoi(gen)
		gen = strconv.Itoa(n + 1)
		if err := h.idx.SetMeta("gen:"+src.ID, gen); err != nil {
			return err
		}
	} else {
		cur, err := h.idx.Meta("cursor:" + src.ID)
		if err != nil {
			return err
		}
		if cur != "" {
			t, err := time.Parse(time.RFC3339, cur)
			if err != nil {
				return fmt.Errorf("stored cursor %q: %w", cur, err)
			}
			from = t.Add(-24 * time.Hour).UTC().Format("2006-01-02T15:04:05Z")
		}
	}

	parse := record.FromDIM
	if src.MetadataPrefix == "oai_dc" {
		parse = record.FromOAIDC
	}
	date, err := c.ListRecords(h.ctx, src.MetadataPrefix, from, func(page []oai.Record) error {
		var ups []record.Record
		var dels []string
		for _, r := range page {
			id := src.ID + ":" + localID(r.Header.Identifier)
			if r.Header.Deleted() {
				dels = append(dels, id)
				continue
			}
			rec, drop, err := parse(r.Metadata.XML)
			if err != nil {
				return fmt.Errorf("%s: %w", r.Header.Identifier, err)
			}
			rec.ID, rec.Source, rec.Modified = id, src.ID, r.Header.Datestamp
			rec.IsPartOf = setNames(r.Header.SetSpecs, sets)
			for _, k := range drop {
				dropped[k]++
			}
			ups = append(ups, rec)
		}
		if err := h.idx.Apply(ups, dels, gen); err != nil {
			return err
		}
		run.Upserted += len(ups)
		run.Deleted += len(dels)
		return nil
	})
	if err != nil {
		return err
	}
	if err := h.idx.SetMeta("cursor:"+src.ID, date); err != nil {
		return err
	}
	if !full {
		return nil
	}
	if seen := run.Upserted + run.Deleted; float64(seen) < 0.9*float64(before) {
		run.Note = fmt.Sprintf("sweep skipped: saw %d records but %d were indexed", seen, before)
		return nil
	}
	run.Swept, err = h.idx.Sweep(src.ID, gen)
	return err
}

func localID(identifier string) string {
	return identifier[strings.LastIndex(identifier, ":")+1:]
}

func setNames(specs []string, sets map[string]string) []string {
	var names []string
	for _, s := range specs {
		if n, ok := sets[s]; ok && n != "" && !slices.Contains(names, n) {
			names = append(names, n)
		}
	}
	return names
}

func (h *Harvester) Status() ([]SourceStatus, error) {
	var out []SourceStatus
	for _, s := range h.sources {
		docs, err := h.idx.Count(s.ID)
		if err != nil {
			return nil, err
		}
		cur, err := h.idx.Meta("cursor:" + s.ID)
		if err != nil {
			return nil, err
		}
		h.mu.Lock()
		st := h.state[s.ID]
		ss := SourceStatus{ID: s.ID, Name: s.Name, Docs: docs, Cursor: cur, Running: st.running, Last: st.last, Dropped: maps.Clone(st.dropped)}
		if h.cron != nil {
			if next := h.cron.Entry(st.entry).Next; !next.IsZero() {
				ss.Next = &next
			}
		}
		h.mu.Unlock()
		out = append(out, ss)
	}
	return out, nil
}
