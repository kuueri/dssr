package record

import (
	"cmp"
	"encoding/xml"
	"strings"
)

type Record struct {
	ID                    string        `json:"id"`
	Source                string        `json:"source"`
	Title                 string        `json:"title,omitempty"`
	Alternative           []string      `json:"alternative,omitempty"`
	Creator               []string      `json:"creator,omitempty"`
	Contributor           []Contributor `json:"contributor,omitempty"`
	Issued                string        `json:"issued,omitempty"`
	Available             string        `json:"available,omitempty"`
	Subject               []string      `json:"subject,omitempty"`
	Abstract              []string      `json:"abstract,omitempty"`
	Description           []string      `json:"description,omitempty"`
	TableOfContents       []string      `json:"tableOfContents,omitempty"`
	Sponsorship           []string      `json:"sponsorship,omitempty"`
	Type                  []string      `json:"type,omitempty"`
	Language              []string      `json:"language,omitempty"`
	Publisher             []string      `json:"publisher,omitempty"`
	BibliographicCitation []string      `json:"bibliographicCitation,omitempty"`
	Identifier            []Identifier  `json:"identifier,omitempty"`
	URL                   string        `json:"url,omitempty"`
	Relation              []string      `json:"relation,omitempty"`
	Rights                []string      `json:"rights,omitempty"`
	License               []string      `json:"license,omitempty"`
	Spatial               []string      `json:"spatial,omitempty"`
	Temporal              []string      `json:"temporal,omitempty"`
	Extent                []string      `json:"extent,omitempty"`
	IsPartOf              []string      `json:"isPartOf,omitempty"`
	Modified              string        `json:"modified,omitempty"`
}

type Contributor struct {
	Name string `json:"name"`
	Role string `json:"role,omitempty"`
}

type Identifier struct {
	Scheme string `json:"scheme,omitempty"`
	Value  string `json:"value"`
}

func (r Record) Year() string {
	if len(r.Issued) < 4 {
		return ""
	}
	for _, c := range r.Issued[:4] {
		if c < '0' || c > '9' {
			return ""
		}
	}
	return r.Issued[:4]
}

type field struct {
	Schema    string `xml:"mdschema,attr"`
	Element   string `xml:"element,attr"`
	Qualifier string `xml:"qualifier,attr"`
	Value     string `xml:",chardata"`
}

func (f field) key() string {
	if f.Qualifier == "" {
		return f.Schema + "." + f.Element
	}
	return f.Schema + "." + f.Element + "." + f.Qualifier
}

func FromDIM(metadata []byte) (Record, []string, error) {
	var d struct {
		Fields []field `xml:"field"`
	}
	if err := xml.Unmarshal(metadata, &d); err != nil {
		return Record{}, nil, err
	}
	var r Record
	var dropped []string
	for _, f := range d.Fields {
		v := strings.TrimSpace(f.Value)
		if v == "" {
			continue
		}
		if !r.add(f, v) {
			dropped = append(dropped, f.key())
		}
	}
	return r, dropped, nil
}

const dcNamespace = "http://purl.org/dc/elements/1.1/"

func FromOAIDC(metadata []byte) (Record, []string, error) {
	var d struct {
		Elements []struct {
			XMLName xml.Name
			Value   string `xml:",chardata"`
		} `xml:",any"`
	}
	if err := xml.Unmarshal(metadata, &d); err != nil {
		return Record{}, nil, err
	}
	var r Record
	var dropped []string
	for _, e := range d.Elements {
		v := strings.TrimSpace(e.Value)
		if v == "" {
			continue
		}
		if e.XMLName.Space != dcNamespace {
			dropped = append(dropped, e.XMLName.Local)
			continue
		}
		f := field{Schema: "dc"}
		f.Element, f.Qualifier, _ = strings.Cut(e.XMLName.Local, "_")
		switch {
		case f.Element == "date" && f.Qualifier == "":
			if r.Issued == "" || v < r.Issued {
				r.Issued = v
			}
			continue
		case f.Element == "identifier" && f.Qualifier == "" && (strings.HasPrefix(v, "http://") || strings.HasPrefix(v, "https://")):
			f.Qualifier = "uri"
		case f.Element == "description" && f.Qualifier == "":
			f.Qualifier = "abstract"
		}
		if !r.add(f, v) {
			dropped = append(dropped, f.key())
		}
	}
	return r, dropped, nil
}

func (r *Record) add(f field, v string) bool {
	if f.Schema != "dc" {
		return false
	}
	q := f.Qualifier
	switch f.Element {
	case "title":
		switch {
		case q == "" && r.Title == "":
			r.Title = v
		case q == "" || q == "alternative":
			r.Alternative = append(r.Alternative, v)
		default:
			return false
		}
	case "contributor":
		if q == "author" {
			r.Creator = append(r.Creator, v)
		} else {
			r.Contributor = append(r.Contributor, Contributor{Name: v, Role: q})
		}
	case "creator":
		if q != "" {
			return false
		}
		r.Creator = append(r.Creator, v)
	case "date":
		switch q {
		case "issued":
			r.Issued = cmp.Or(r.Issued, v)
		case "available":
			r.Available = cmp.Or(r.Available, v)
		default:
			return false
		}
	case "subject":
		r.Subject = append(r.Subject, v)
	case "description":
		switch q {
		case "":
			r.Description = append(r.Description, v)
		case "abstract":
			r.Abstract = append(r.Abstract, v)
		case "tableofcontents":
			r.TableOfContents = append(r.TableOfContents, v)
		case "sponsorship":
			r.Sponsorship = append(r.Sponsorship, v)
		default:
			return false
		}
	case "type", "publisher":
		if q != "" {
			return false
		}
		if f.Element == "type" {
			r.Type = append(r.Type, v)
		} else {
			r.Publisher = append(r.Publisher, v)
		}
	case "language":
		if q != "" && q != "iso" {
			return false
		}
		r.Language = append(r.Language, v)
	case "identifier":
		switch {
		case q == "uri" && r.URL == "":
			r.URL = v
		case q == "citation":
			r.BibliographicCitation = append(r.BibliographicCitation, v)
		default:
			r.Identifier = append(r.Identifier, Identifier{Scheme: q, Value: v})
		}
	case "relation":
		r.Relation = append(r.Relation, v)
	case "rights":
		if q == "uri" {
			r.License = append(r.License, v)
		} else {
			r.Rights = append(r.Rights, v)
		}
	case "coverage":
		switch q {
		case "spatial":
			r.Spatial = append(r.Spatial, v)
		case "temporal":
			r.Temporal = append(r.Temporal, v)
		default:
			return false
		}
	case "format":
		if q != "extent" {
			return false
		}
		r.Extent = append(r.Extent, v)
	default:
		return false
	}
	return true
}
