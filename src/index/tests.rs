use super::search::Facet;
use super::*;
use crate::record::Contributor;

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn rec(id: &str, title: &str) -> Record {
    Record {
        id: id.into(),
        source: id.split(':').next().unwrap().into(),
        title: title.into(),
        ..Default::default()
    }
}

fn fixtures() -> Vec<Record> {
    vec![
        Record {
            creator: strings(&["Rahma, Siti"]),
            issued: "2020".into(),
            subject: strings(&["wisata ziarah"]),
            r#abstract: strings(&[
                "Penelitian ini menganalisis potensi wisata ziarah dan pendapatan daerah.",
            ]),
            r#type: strings(&["Thesis"]),
            language: strings(&["id"]),
            is_part_of: strings(&["Fakultas Ekonomi"]),
            ..rec("repo:1/1", "Analisis potensi wisata ziarah Malang Raya")
        },
        Record {
            creator: strings(&["Hidayat, Arif"]),
            issued: "2018-03-01".into(),
            subject: strings(&["pakan ternak"]),
            contributor: vec![Contributor {
                name: "Santoso, Budi".into(),
                role: "advisor".into(),
            }],
            r#type: strings(&["Thesis"]),
            language: strings(&["id"]),
            is_part_of: strings(&["Fakultas Peternakan"]),
            ..rec("repo:1/2", "Pengaruh pakan jerami padi terhadap bobot sapi")
        },
        Record {
            creator: strings(&["Putri, Ayu"]),
            issued: "2023".into(),
            subject: strings(&["pemasaran"]),
            r#type: strings(&["Article"]),
            language: strings(&["en"]),
            is_part_of: strings(&["Fakultas Ekonomi"]),
            ..rec("repo:1/3", "Strategi pemasaran wisata halal")
        },
    ]
}

pub struct Temp(PathBuf);

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn temp_dir() -> (Temp, PathBuf) {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("dssr-test-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    (Temp(dir.clone()), dir)
}

fn open() -> (Temp, Index) {
    let (t, dir) = temp_dir();
    let x = Index::open(&dir.join("nested").join("index")).unwrap();
    x.apply(&fixtures(), &[], "1").unwrap();
    (t, x)
}

fn query(q: &str) -> SearchQuery {
    SearchQuery {
        q: q.into(),
        page: 1,
        limit: 10,
        ..Default::default()
    }
}

fn ids(r: &SearchResult) -> String {
    r.results
        .iter()
        .map(|h| h.record.id.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

#[test]
fn search() {
    let (_t, x) = open();
    let cases: Vec<(&str, SearchQuery, &str)> = vec![
        ("and across fields", query("wisata rahma"), "repo:1/1"),
        (
            "title boost ranks first",
            query("wisata"),
            "repo:1/1,repo:1/3",
        ),
        ("contributor", query("santoso"), "repo:1/2"),
        (
            "stop words ignored",
            query("wisata dan yang the"),
            "repo:1/1,repo:1/3",
        ),
        ("fuzzy one edit", query("jeramy"), "repo:1/2"),
        ("affix variant", query("memasarkan"), "repo:1/3"),
        ("phrase", query("\"wisata halal\""), "repo:1/3"),
        ("phrase in subject", query("\"pakan ternak\""), "repo:1/2"),
        ("phrase in creator", query("\"rahma siti\""), "repo:1/1"),
        ("or fallback", query("ziarah ternak"), "repo:1/1,repo:1/2"),
        (
            "type filter",
            SearchQuery {
                types: strings(&["Article"]),
                ..query("wisata")
            },
            "repo:1/3",
        ),
        (
            "collection filter",
            SearchQuery {
                collections: strings(&["Fakultas Peternakan"]),
                ..query("pakan wisata")
            },
            "repo:1/2",
        ),
        (
            "year range",
            SearchQuery {
                year_from: "2021".into(),
                year_to: "2024".into(),
                ..query("wisata")
            },
            "repo:1/3",
        ),
        (
            "newest",
            SearchQuery {
                newest: true,
                ..query("wisata")
            },
            "repo:1/3,repo:1/1",
        ),
        ("only stop words", query("yang dan"), ""),
        ("no match", query("kuantum"), ""),
    ];
    for (name, q, want) in cases {
        let got = ids(&x.search(&q).unwrap());
        assert!(
            got.starts_with(want) && (!want.is_empty() || got.is_empty()),
            "{name}: got {got:?}, want prefix {want:?}"
        );
    }
}

#[test]
fn source_filter() {
    let (_t, x) = open();
    x.apply(&[rec("slims:9", "Wisata religi")], &[], "1")
        .unwrap();
    for (sources, want) in [
        (vec!["slims"], "slims:9"),
        (vec!["repo"], "repo:1/1,repo:1/3"),
        (vec!["repo", "slims"], "repo:1/1,repo:1/3,slims:9"),
        (vec!["nope"], ""),
    ] {
        let mut got: Vec<String> = x
            .search(&SearchQuery {
                sources: strings(&sources),
                ..query("wisata")
            })
            .unwrap()
            .results
            .into_iter()
            .map(|h| h.record.id)
            .collect();
        got.sort();
        assert_eq!(got.join(","), want, "{sources:?}");
    }
}

#[test]
fn most_words() {
    let (_t, x) = open();
    x.apply(
        &[
            rec("slims:1", "Hukum acara pidana"),
            Record {
                r#abstract: strings(&["Penulis mencari buku hukum acara pidana di perpustakaan."]),
                ..rec("repo:2/1", "Pemalsuan data wali nikah")
            },
        ],
        &[],
        "1",
    )
    .unwrap();
    for (q, inside, outside) in [
        ("cari buku hukum acara pidana", "slims:1", ""),
        ("buku hukum pidana", "repo:2/1", "slims:1"),
        (
            "\"wali nikah\" cari buku hukum acara",
            "repo:2/1",
            "slims:1",
        ),
    ] {
        let got = ids(&x.search(&query(q)).unwrap());
        assert!(
            got.contains(inside) && (outside.is_empty() || !got.contains(outside)),
            "{q}: got {got:?}"
        );
    }
}

#[test]
fn title_phrase() {
    let (_t, x) = open();
    x.apply(
        &[
            rec("slims:10", "Bank dan lembaga keuangan lain: teori, praktik, regulasi, dan perkembangannya di Indonesia"),
            rec("slims:11", "Lembaga keuangan bank"),
            rec("slims:12", "Bank lembaga perkreditan rakyat keuangan"),
        ],
        &[],
        "1",
    )
    .unwrap();
    assert!(ids(&x.search(&query("bank lembaga keuangan")).unwrap()).starts_with("slims:10,"));
    assert_eq!(
        ids(&x.search(&query("\"bank lembaga\"")).unwrap()),
        "slims:12"
    );
}

#[test]
fn scores_ignore_segments() {
    let scores = |batches: Vec<Vec<Record>>| {
        let (_t, dir) = temp_dir();
        let x = Index::open(&dir).unwrap();
        for b in batches {
            x.apply(&b, &[], "1").unwrap();
        }
        x.search(&query("wisata"))
            .unwrap()
            .results
            .iter()
            .map(|h| h.score)
            .collect::<Vec<_>>()
    };
    let f = fixtures();
    let one = scores(vec![f.clone()]);
    let many = scores(vec![f[..1].to_vec(), f[1..2].to_vec(), f[2..].to_vec()]);
    assert!(!one.is_empty());
    assert_eq!(one, many);
}

#[test]
fn ties_sort_by_id() {
    for order in [["slims:9", "slims:8"], ["slims:8", "slims:9"]] {
        let (_t, x) = open();
        for id in order {
            x.apply(&[rec(id, "Hukum lingkungan")], &[], "1").unwrap();
        }
        for newest in [false, true] {
            let got = ids(&x
                .search(&SearchQuery {
                    newest,
                    ..query("hukum lingkungan")
                })
                .unwrap());
            assert_eq!(got, "slims:8,slims:9", "indexed {order:?}, newest {newest}");
        }
    }
}

#[test]
fn reopen() {
    let (_t, dir) = temp_dir();
    Index::open(&dir)
        .unwrap()
        .apply(&fixtures(), &[], "1")
        .unwrap();
    let x = Index::open(&dir).unwrap();
    assert_eq!(
        ids(&x.search(&query("wisata")).unwrap()),
        "repo:1/1,repo:1/3"
    );
}

#[test]
fn refuses_legacy_index() {
    let (_t, dir) = temp_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("index_meta.json"), "{}").unwrap();
    assert!(
        Index::open(&dir)
            .err()
            .unwrap()
            .to_string()
            .contains("reindex")
    );
}

#[test]
fn result_shape() {
    let (_t, x) = open();
    let r = x.search(&query("wisata")).unwrap();
    let h = &r.results[0];
    assert!(h.score > 0.0 && h.record.creator[0] == "Rahma, Siti");
    assert!(
        h.highlight["title"]
            .join("")
            .contains("<mark>wisata</mark>"),
        "{:?}",
        h.highlight
    );
    assert_eq!(
        r.facets["isPartOf"][0],
        Facet {
            value: "Fakultas Ekonomi".into(),
            count: 2
        }
    );
    assert_eq!(r.facets["year"].len(), 2);
}

#[test]
fn paging() {
    let (_t, x) = open();
    let r = x
        .search(&SearchQuery {
            page: 2,
            limit: 1,
            ..query("wisata")
        })
        .unwrap();
    assert_eq!((r.total, ids(&r).as_str()), (2, "repo:1/3"));
}

#[test]
fn purge_removes_source() {
    let (t, dir) = temp_dir();
    let path = dir.join("index");
    let x = Index::open(&path).unwrap();
    x.apply(&fixtures(), &[], "1").unwrap();
    x.apply(&[rec("slims:9", "Katalog")], &[], "1").unwrap();
    for k in ["cursor:repo", "gen:repo", "suspended:repo", "cursor:slims"] {
        x.set_meta(k, "v").unwrap();
    }
    assert_eq!(x.purge("repo").unwrap(), 3);
    assert_eq!(
        (x.count("repo").unwrap(), x.count("slims").unwrap()),
        (0, 1)
    );
    for k in ["cursor:repo", "gen:repo", "suspended:repo"] {
        assert_eq!(x.meta(k), "", "{k}");
    }
    assert_eq!(x.meta("cursor:slims"), "v");
    let snap = x.current();
    assert_eq!(snap.searcher.segment_readers().len(), 1);
    assert!(
        snap.searcher
            .segment_readers()
            .iter()
            .all(|s| s.num_deleted_docs() == 0)
    );
    drop(x);
    let x = Index::open(&path).unwrap();
    assert_eq!(
        (x.count("repo").unwrap(), x.count("slims").unwrap()),
        (0, 1)
    );
    assert_eq!(x.purge("nope").unwrap(), 0);
    drop(t);
}

#[test]
fn sources_lists_only_live() {
    let (_t, x) = open();
    x.apply(&[rec("slims:9", "Katalog")], &[], "1").unwrap();
    assert_eq!(x.sources().unwrap(), ["repo", "slims"]);
    x.apply(&[], &["slims:9".into()], "1").unwrap();
    assert_eq!(x.sources().unwrap(), ["repo"]);
}

#[test]
fn get_count_sweep_meta() {
    let (_t, x) = open();
    assert_eq!(
        x.get("repo:1/2").unwrap().unwrap().contributor[0].role,
        "advisor"
    );
    assert!(x.get("repo:missing").unwrap().is_none());
    x.apply(&fixtures()[..1], &["repo:1/3".to_string()], "2")
        .unwrap();
    assert_eq!(x.count("repo").unwrap(), 2);
    assert_eq!(x.sweep("repo", "2").unwrap(), 1);
    assert!(x.get("repo:1/1").unwrap().is_some());
    x.set_meta("cursor:repo", "2026-10-26T05:00:00Z").unwrap();
    assert_eq!(x.meta("cursor:repo"), "2026-10-26T05:00:00Z");
}
