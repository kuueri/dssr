package index

import "testing"

func TestStem(t *testing.T) {
	cases := map[string]string{
		"pembelajaran": "ajar",
		"belajar":      "ajar",
		"perkantoran":  "kantor",
		"beragama":     "agama",
		"pembatalan":   "batal",
		"perkawinan":   "kawin",
		"penulisan":    "tulis",
		"menyapu":      "sapu",
		"memasarkan":   "asar",
		"pemasaran":    "asar",
		"menganalisis": "analisis",
		"bukunya":      "buku",
		"bacalah":      "baca",
		"malang":       "malang",
		"2017":         "2017",
	}
	for in, want := range cases {
		if got := stem(in); got != want {
			t.Errorf("stem(%q) = %q, want %q", in, got, want)
		}
	}
}
