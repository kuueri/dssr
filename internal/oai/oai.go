package oai

import (
	"context"
	"encoding/xml"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"time"
)

type Client struct {
	BaseURL   string
	UserAgent string
	HTTP      *http.Client
	Retries   int
	Backoff   time.Duration
}

func New(baseURL, userAgent string) *Client {
	return &Client{
		BaseURL:   baseURL,
		UserAgent: userAgent,
		HTTP:      &http.Client{Timeout: 60 * time.Second},
		Retries:   3,
		Backoff:   2 * time.Second,
	}
}

type Header struct {
	Identifier string   `xml:"identifier"`
	Datestamp  string   `xml:"datestamp"`
	SetSpecs   []string `xml:"setSpec"`
	Status     string   `xml:"status,attr"`
}

func (h Header) Deleted() bool { return h.Status == "deleted" }

type Record struct {
	Header   Header `xml:"header"`
	Metadata struct {
		XML []byte `xml:",innerxml"`
	} `xml:"metadata"`
}

type Error struct {
	Code    string `xml:"code,attr"`
	Message string `xml:",chardata"`
}

func (e *Error) Error() string { return "oai-pmh " + e.Code + ": " + strings.TrimSpace(e.Message) }

type response struct {
	ResponseDate string  `xml:"responseDate"`
	Errors       []Error `xml:"error"`
	ListRecords  struct {
		Records []Record `xml:"record"`
		Token   string   `xml:"resumptionToken"`
	} `xml:"ListRecords"`
	ListSets struct {
		Sets []struct {
			Spec string `xml:"setSpec"`
			Name string `xml:"setName"`
		} `xml:"set"`
		Token string `xml:"resumptionToken"`
	} `xml:"ListSets"`
}

func (c *Client) ListRecords(ctx context.Context, prefix, from string, page func([]Record) error) (string, error) {
	q := url.Values{"verb": {"ListRecords"}, "metadataPrefix": {prefix}}
	if from != "" {
		q.Set("from", from)
	}
	var date string
	for {
		r, err := c.get(ctx, q)
		if err != nil {
			return "", err
		}
		if date == "" {
			date = r.ResponseDate
		}
		if len(r.Errors) > 0 {
			if r.Errors[0].Code == "noRecordsMatch" {
				return date, nil
			}
			return "", &r.Errors[0]
		}
		if err := page(r.ListRecords.Records); err != nil {
			return "", err
		}
		tok := strings.TrimSpace(r.ListRecords.Token)
		if tok == "" {
			return date, nil
		}
		q = url.Values{"verb": {"ListRecords"}, "resumptionToken": {tok}}
	}
}

func (c *Client) ListSets(ctx context.Context) (map[string]string, error) {
	sets := map[string]string{}
	q := url.Values{"verb": {"ListSets"}}
	for {
		r, err := c.get(ctx, q)
		if err != nil {
			return nil, err
		}
		if len(r.Errors) > 0 {
			if r.Errors[0].Code == "noSetHierarchy" {
				return sets, nil
			}
			return nil, &r.Errors[0]
		}
		for _, s := range r.ListSets.Sets {
			sets[s.Spec] = strings.TrimSpace(s.Name)
		}
		tok := strings.TrimSpace(r.ListSets.Token)
		if tok == "" {
			return sets, nil
		}
		q = url.Values{"verb": {"ListSets"}, "resumptionToken": {tok}}
	}
}

type retryable struct {
	err   error
	after time.Duration
}

func (e *retryable) Error() string { return e.err.Error() }

func (c *Client) get(ctx context.Context, q url.Values) (*response, error) {
	u := c.BaseURL + "?" + q.Encode()
	for attempt := 0; ; attempt++ {
		r, err := c.try(ctx, u)
		var re *retryable
		if err == nil || !errors.As(err, &re) || attempt == c.Retries {
			return r, err
		}
		wait := re.after
		if wait == 0 {
			wait = c.Backoff << attempt
		}
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		case <-time.After(wait):
		}
	}
}

func (c *Client) try(ctx context.Context, u string) (*response, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, u, nil)
	if err != nil {
		return nil, err
	}
	req.Header.Set("User-Agent", c.UserAgent)
	res, err := c.HTTP.Do(req)
	if err != nil {
		if ctx.Err() != nil {
			return nil, err
		}
		return nil, &retryable{err: err}
	}
	defer res.Body.Close()
	if res.StatusCode >= 500 {
		return nil, &retryable{err: fmt.Errorf("GET %s: %s", u, res.Status), after: retryAfter(res.Header.Get("Retry-After"))}
	}
	if res.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("GET %s: %s", u, res.Status)
	}
	var r response
	if err := xml.NewDecoder(res.Body).Decode(&r); err != nil {
		return nil, &retryable{err: fmt.Errorf("GET %s: %w", u, err)}
	}
	return &r, nil
}

func retryAfter(v string) time.Duration {
	var d time.Duration
	if s, err := strconv.Atoi(v); err == nil && s > 0 {
		d = time.Duration(s) * time.Second
	} else if t, err := http.ParseTime(v); err == nil {
		d = time.Until(t)
	}
	return min(max(d, 0), 10*time.Minute)
}
