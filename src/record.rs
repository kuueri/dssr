use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Record {
    pub id: String,
    pub source: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub alternative: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub creator: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub contributor: Vec<Contributor>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub issued: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub available: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub subject: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub r#abstract: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub description: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub table_of_contents: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sponsorship: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub r#type: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub language: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub publisher: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bibliographic_citation: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub identifier: Vec<Identifier>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub url: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub relation: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rights: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub license: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub spatial: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub temporal: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extent: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub is_part_of: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub modified: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Contributor {
    pub name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub role: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Identifier {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub scheme: String,
    pub value: String,
}

impl Record {
    pub fn year(&self) -> &str {
        match self.issued.get(..4) {
            Some(y) if y.bytes().all(|c| c.is_ascii_digit()) => y,
            _ => "",
        }
    }

    fn add(&mut self, schema: &str, element: &str, q: &str, v: &str) -> bool {
        if schema != "dc" {
            return false;
        }
        let v = v.to_string();
        match element {
            "title" => match q {
                "" if self.title.is_empty() => self.title = v,
                "" | "alternative" => self.alternative.push(v),
                _ => return false,
            },
            "contributor" => {
                if q == "author" {
                    self.creator.push(v);
                } else {
                    self.contributor.push(Contributor {
                        name: v,
                        role: q.to_string(),
                    });
                }
            }
            "creator" => {
                if !q.is_empty() {
                    return false;
                }
                self.creator.push(v);
            }
            "date" => match q {
                "issued" if self.issued.is_empty() => self.issued = v,
                "available" if self.available.is_empty() => self.available = v,
                "issued" | "available" => {}
                _ => return false,
            },
            "subject" => self.subject.push(v),
            "description" => match q {
                "" => self.description.push(v),
                "abstract" => self.r#abstract.push(v),
                "tableofcontents" => self.table_of_contents.push(v),
                "sponsorship" => self.sponsorship.push(v),
                _ => return false,
            },
            "type" | "publisher" => {
                if !q.is_empty() {
                    return false;
                }
                if element == "type" {
                    self.r#type.push(v);
                } else {
                    self.publisher.push(v);
                }
            }
            "language" => {
                if !q.is_empty() && q != "iso" {
                    return false;
                }
                self.language.push(v);
            }
            "identifier" => match q {
                "uri" if self.url.is_empty() => self.url = v,
                "citation" => self.bibliographic_citation.push(v),
                _ => self.identifier.push(Identifier {
                    scheme: q.to_string(),
                    value: v,
                }),
            },
            "relation" => self.relation.push(v),
            "rights" => {
                if q == "uri" {
                    self.license.push(v);
                } else {
                    self.rights.push(v);
                }
            }
            "coverage" => match q {
                "spatial" => self.spatial.push(v),
                "temporal" => self.temporal.push(v),
                _ => return false,
            },
            "format" => {
                if q != "extent" {
                    return false;
                }
                self.extent.push(v);
            }
            _ => return false,
        }
        true
    }
}

fn key(schema: &str, element: &str, qualifier: &str) -> String {
    if qualifier.is_empty() {
        format!("{schema}.{element}")
    } else {
        format!("{schema}.{element}.{qualifier}")
    }
}

fn chardata(n: roxmltree::Node) -> String {
    n.children()
        .filter(|c| c.is_text())
        .filter_map(|c| c.text())
        .collect()
}

pub fn from_dim(root: roxmltree::Node) -> (Record, Vec<String>) {
    let mut r = Record::default();
    let mut dropped = Vec::new();
    for f in root
        .children()
        .filter(|c| c.is_element() && c.tag_name().name() == "field")
    {
        let text = chardata(f);
        let v = text.trim();
        if v.is_empty() {
            continue;
        }
        let (schema, element, qualifier) = (
            f.attribute("mdschema").unwrap_or(""),
            f.attribute("element").unwrap_or(""),
            f.attribute("qualifier").unwrap_or(""),
        );
        if !r.add(schema, element, qualifier, v) {
            dropped.push(key(schema, element, qualifier));
        }
    }
    (r, dropped)
}

const DC_NAMESPACE: &str = "http://purl.org/dc/elements/1.1/";

pub fn from_oai_dc(root: roxmltree::Node) -> (Record, Vec<String>) {
    let mut r = Record::default();
    let mut dropped = Vec::new();
    for e in root.children().filter(|c| c.is_element()) {
        let text = chardata(e);
        let v = text.trim();
        if v.is_empty() {
            continue;
        }
        let local = e.tag_name().name();
        if e.tag_name().namespace() != Some(DC_NAMESPACE) {
            dropped.push(local.to_string());
            continue;
        }
        let (element, mut qualifier) = local.split_once('_').unwrap_or((local, ""));
        if element == "date" && qualifier.is_empty() {
            if r.issued.is_empty() || v < r.issued.as_str() {
                r.issued = v.to_string();
            }
            continue;
        }
        if element == "identifier"
            && qualifier.is_empty()
            && (v.starts_with("http://") || v.starts_with("https://"))
        {
            qualifier = "uri";
        } else if element == "description" && qualifier.is_empty() {
            qualifier = "abstract";
        }
        if !r.add("dc", element, qualifier, v) {
            dropped.push(key("dc", element, qualifier));
        }
    }
    (r, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(path: &str, f: fn(roxmltree::Node) -> (Record, Vec<String>)) -> (Record, Vec<String>) {
        let text = std::fs::read_to_string(path).unwrap();
        let doc = roxmltree::Document::parse(&text).unwrap();
        let (r, mut dropped) = f(doc.root_element());
        dropped.sort();
        (r, dropped)
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn dim() {
        let (r, dropped) = parse("testdata/record/dim.xml", from_dim);
        let want = Record {
            title: "Analisis potensi wisata ziarah Malang Raya".into(),
            alternative: strings(&["Pilgrimage tourism in Greater Malang"]),
            creator: strings(&["Example, Author"]),
            contributor: vec![
                Contributor {
                    name: "Example, Advisor".into(),
                    role: "advisor".into(),
                },
                Contributor {
                    name: "Example, Contributor".into(),
                    role: "".into(),
                },
            ],
            issued: "2020".into(),
            available: "2020-11-06T02:48:52Z".into(),
            subject: strings(&["wisata ziarah", "338.4"]),
            r#abstract: strings(&[
                "Penelitian ini menganalisis potensi wisata ziarah.",
                "This study analyzes pilgrimage tourism.",
            ]),
            sponsorship: strings(&["Hibah penelitian"]),
            r#type: strings(&["Thesis"]),
            language: strings(&["id"]),
            publisher: strings(&["Universitas Islam Malang"]),
            bibliographic_citation: strings(&["Author (2020). Analisis potensi wisata ziarah."]),
            identifier: vec![Identifier {
                scheme: "isbn".into(),
                value: "978-602-0000-00-0".into(),
            }],
            url: "https://repository.example.ac.id/handle/123456789/56".into(),
            relation: strings(&["Skripsi;"]),
            rights: strings(&["Open access"]),
            license: strings(&["https://creativecommons.org/licenses/by/4.0/"]),
            spatial: strings(&["Malang"]),
            extent: strings(&["120 p."]),
            ..Default::default()
        };
        assert_eq!(r, want);
        assert_eq!(
            dropped,
            [
                "dc.date.accessioned",
                "dc.description.provenance",
                "dc.format.mimetype",
                "local.nim"
            ]
        );
        assert_eq!(r.year(), "2020");
    }

    #[test]
    fn oai_dc() {
        let (r, dropped) = parse("testdata/record/oai_dc.xml", from_oai_dc);
        let want = Record {
            title: "Pengantar ilmu perpustakaan".into(),
            creator: strings(&["Example, Author"]),
            issued: "1991".into(),
            subject: strings(&["Perpustakaan", "020"]),
            r#abstract: strings(&["Bibliografi: hlm. 517-524"]),
            r#type: strings(&["Text"]),
            language: strings(&["id"]),
            publisher: strings(&["Example Publisher"]),
            identifier: vec![
                Identifier {
                    scheme: "isbn".into(),
                    value: "9794030449".into(),
                },
                Identifier {
                    scheme: "".into(),
                    value: "020 EXA p".into(),
                },
            ],
            url: "https://catalog.example.org/index.php?p=show_detail&id=1".into(),
            ..Default::default()
        };
        assert_eq!(r, want);
        assert_eq!(dropped, ["dc.coverage", "dc.format", "image"]);
    }

    #[test]
    fn year() {
        for (issued, want) in [("2019-05-01", "2019"), ("20", ""), ("n.d.", ""), ("", "")] {
            assert_eq!(
                Record {
                    issued: issued.into(),
                    ..Default::default()
                }
                .year(),
                want
            );
        }
    }
}
