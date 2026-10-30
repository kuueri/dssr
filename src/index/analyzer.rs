use std::collections::HashSet;
use std::sync::LazyLock;

use tantivy::tokenizer::{Token, TokenStream, Tokenizer};
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

use super::stem::stem;

pub const ANALYZER: &str = "dssr";
pub const PHRASE_ANALYZER: &str = "dssr_phrase";

const CONTENT_WORDS: &[&str] = &[
    "banyak",
    "bermacam",
    "bersama",
    "biasa",
    "bukan",
    "dahulu",
    "dekat",
    "depan",
    "dini",
    "diri",
    "dirinya",
    "dulu",
    "hal",
    "ibarat",
    "kecil",
    "kini",
    "lain",
    "lainnya",
    "lalu",
    "lama",
    "lamanya",
    "macam",
    "mampu",
    "nanti",
    "pantas",
    "percuma",
    "saat",
    "saatnya",
    "sama",
    "sedikit",
    "sekarang",
    "sekitar",
    "sela",
    "sendiri",
    "sendirinya",
    "serupa",
    "sesama",
    "tadi",
    "tak",
    "tanpa",
    "tertentu",
    "tidak",
];

fn word_list(data: &str) -> HashSet<String> {
    data.lines()
        .flat_map(|line| {
            line.split(['#', '|'])
                .next()
                .unwrap_or("")
                .split_whitespace()
        })
        .map(str::to_string)
        .collect()
}

static STOP_EN: LazyLock<HashSet<String>> =
    LazyLock::new(|| word_list(include_str!("stop_words_en.txt")));

static STOP_ID: LazyLock<HashSet<String>> = LazyLock::new(|| {
    let mut words = word_list(include_str!("stop_words_id.txt"));
    for w in CONTENT_WORDS {
        words.remove(*w);
    }
    words
});

fn lowercase(s: &str) -> String {
    let mut out: String = s.chars().flat_map(char::to_lowercase).collect();
    if out.ends_with('σ') && s.ends_with('Σ') {
        out.pop();
        out.push('ς');
    }
    out
}

pub fn analyze(text: &str, gapless: bool) -> Vec<Token> {
    let mut out = Vec::new();
    let mut position = 0;
    for (start, word) in text.split_word_bound_indices() {
        if !word.chars().any(char::is_alphanumeric) {
            continue;
        }
        position += 1;
        let term = lowercase(&word.nfkc().collect::<String>());
        if STOP_EN.contains(&term) || STOP_ID.contains(&term) {
            continue;
        }
        out.push(Token {
            offset_from: start,
            offset_to: start + word.len(),
            position: if gapless { out.len() } else { position - 1 },
            text: stem(&term),
            position_length: 1,
        });
    }
    out
}

#[derive(Clone)]
pub struct DssrTokenizer {
    pub gapless: bool,
}

pub struct DssrTokenStream {
    tokens: Vec<Token>,
    next: usize,
}

impl Tokenizer for DssrTokenizer {
    type TokenStream<'a> = DssrTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> DssrTokenStream {
        DssrTokenStream {
            tokens: analyze(text, self.gapless),
            next: 0,
        }
    }
}

impl TokenStream for DssrTokenStream {
    fn advance(&mut self) -> bool {
        self.next += 1;
        self.next <= self.tokens.len()
    }

    fn token(&self) -> &Token {
        &self.tokens[self.next - 1]
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.tokens[self.next - 1]
    }
}

#[cfg(test)]
mod tests {
    use super::analyze;

    fn render(gapless: bool) -> String {
        analyze("konsep dan diri of islam", gapless)
            .iter()
            .map(|t| format!("{}@{}", t.text, t.position + 1))
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn positions() {
        assert_eq!(render(false), "konsep@1 diri@3 islam@5");
        assert_eq!(render(true), "konsep@1 diri@2 islam@3");
    }

    #[test]
    fn content_words_stay() {
        let terms: Vec<_> = analyze("Anak usia dini, yang KECIL", false)
            .into_iter()
            .map(|t| t.text)
            .collect();
        assert_eq!(terms, ["anak", "usia", "dini", "kecil"]);
    }
}
