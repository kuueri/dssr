package record

import (
	"os"
	"reflect"
	"slices"
	"testing"
)

func TestFromDIM(t *testing.T) {
	b, err := os.ReadFile("testdata/dim.xml")
	if err != nil {
		t.Fatal(err)
	}
	r, dropped, err := FromDIM(b)
	if err != nil {
		t.Fatal(err)
	}
	want := Record{
		Title:                 "Analisis potensi wisata ziarah Malang Raya",
		Alternative:           []string{"Pilgrimage tourism in Greater Malang"},
		Creator:               []string{"Example, Author"},
		Contributor:           []Contributor{{"Example, Advisor", "advisor"}, {"Example, Contributor", ""}},
		Issued:                "2020",
		Available:             "2020-11-06T02:48:52Z",
		Subject:               []string{"wisata ziarah", "338.4"},
		Abstract:              []string{"Penelitian ini menganalisis potensi wisata ziarah.", "This study analyzes pilgrimage tourism."},
		Sponsorship:           []string{"Hibah penelitian"},
		Type:                  []string{"Thesis"},
		Language:              []string{"id"},
		Publisher:             []string{"Universitas Islam Malang"},
		BibliographicCitation: []string{"Author (2020). Analisis potensi wisata ziarah."},
		Identifier:            []Identifier{{"isbn", "978-602-0000-00-0"}},
		URL:                   "https://repository.example.ac.id/handle/123456789/56",
		Relation:              []string{"Skripsi;"},
		Rights:                []string{"Open access"},
		License:               []string{"https://creativecommons.org/licenses/by/4.0/"},
		Spatial:               []string{"Malang"},
		Extent:                []string{"120 p."},
	}
	if !reflect.DeepEqual(r, want) {
		t.Errorf("record\n got %+v\nwant %+v", r, want)
	}
	slices.Sort(dropped)
	if w := []string{"dc.date.accessioned", "dc.description.provenance", "dc.format.mimetype", "local.nim"}; !slices.Equal(dropped, w) {
		t.Errorf("dropped %v, want %v", dropped, w)
	}
	if r.Year() != "2020" {
		t.Errorf("year %q", r.Year())
	}
}

func TestYear(t *testing.T) {
	for issued, want := range map[string]string{"2019-05-01": "2019", "20": "", "n.d.": "", "": ""} {
		if got := (Record{Issued: issued}).Year(); got != want {
			t.Errorf("Year(%q) = %q, want %q", issued, got, want)
		}
	}
}
