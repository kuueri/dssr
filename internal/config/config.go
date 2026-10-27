package config

import (
	"bytes"
	"encoding/hex"
	"errors"
	"fmt"
	"net/url"
	"os"
	"regexp"
	"time"
	_ "time/tzdata"

	"github.com/robfig/cron/v3"
	"go.yaml.in/yaml/v3"
)

type Config struct {
	Listen      string   `yaml:"listen"`
	IndexPath   string   `yaml:"index_path"`
	AdminSocket string   `yaml:"admin_socket"`
	Timezone    string   `yaml:"timezone"`
	Clients     []Client `yaml:"clients"`
	Sources     []Source `yaml:"sources"`

	Location *time.Location `yaml:"-"`
}

type Client struct {
	Name        string `yaml:"name"`
	TokenSHA256 string `yaml:"token_sha256"`
}

type Source struct {
	ID             string `yaml:"id"`
	Name           string `yaml:"name"`
	OAIURL         string `yaml:"oai_url"`
	MetadataPrefix string `yaml:"metadata_prefix"`
	Schedule       string `yaml:"schedule"`
}

var sourceID = regexp.MustCompile(`^[a-z0-9][a-z0-9-]*$`)

func Load(path string) (*Config, error) {
	b, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	dec := yaml.NewDecoder(bytes.NewReader(b))
	dec.KnownFields(true)
	var c Config
	if err := dec.Decode(&c); err != nil {
		return nil, fmt.Errorf("%s: %w", path, err)
	}
	if err := c.validate(); err != nil {
		return nil, fmt.Errorf("%s: %w", path, err)
	}
	return &c, nil
}

func (c *Config) validate() error {
	if c.Listen == "" || c.IndexPath == "" || c.AdminSocket == "" {
		return errors.New("listen, index_path, and admin_socket are required")
	}
	if len(c.AdminSocket) > 107 {
		return errors.New("admin_socket path must be at most 107 bytes, the unix socket limit")
	}
	loc, err := time.LoadLocation(c.Timezone)
	if err != nil {
		return fmt.Errorf("timezone: %w", err)
	}
	c.Location = loc
	for _, cl := range c.Clients {
		if h, err := hex.DecodeString(cl.TokenSHA256); err != nil || len(h) != 32 {
			return fmt.Errorf("client %q: token_sha256 must be 64 hex characters", cl.Name)
		}
	}
	if len(c.Sources) == 0 {
		return errors.New("at least one source is required")
	}
	seen := map[string]bool{}
	for i := range c.Sources {
		s := &c.Sources[i]
		if !sourceID.MatchString(s.ID) {
			return fmt.Errorf("source id %q must match %s", s.ID, sourceID)
		}
		if seen[s.ID] {
			return fmt.Errorf("source id %q is duplicated", s.ID)
		}
		seen[s.ID] = true
		if u, err := url.Parse(s.OAIURL); err != nil || (u.Scheme != "http" && u.Scheme != "https") || u.Host == "" {
			return fmt.Errorf("source %q: oai_url must be an http or https URL", s.ID)
		}
		if s.MetadataPrefix == "" {
			s.MetadataPrefix = "oai_dc"
		}
		if s.MetadataPrefix != "dim" && s.MetadataPrefix != "oai_dc" {
			return fmt.Errorf("source %q: metadata_prefix must be dim or oai_dc, the mapped formats", s.ID)
		}
		if _, err := cron.ParseStandard(s.Schedule); err != nil {
			return fmt.Errorf("source %q: schedule: %w", s.ID, err)
		}
	}
	return nil
}
