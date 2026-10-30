const REMOVED_KE: u32 = 1;
const REMOVED_PENG: u32 = 1 << 1;
const REMOVED_DI: u32 = 1 << 2;
const REMOVED_MENG: u32 = 1 << 3;
const REMOVED_TER: u32 = 1 << 4;
const REMOVED_BER: u32 = 1 << 5;
const REMOVED_PE: u32 = 1 << 6;

struct State {
    s: String,
    syllables: usize,
    flags: u32,
}

fn is_vowel(c: u8) -> bool {
    b"aeiou".contains(&c)
}

pub fn stem(s: &str) -> String {
    let mut w = State {
        s: s.to_string(),
        syllables: s.bytes().filter(|&c| is_vowel(c)).count(),
        flags: 0,
    };
    w.particle();
    w.possessive();
    let n = w.s.len();
    w.first_order_prefix();
    if w.s.len() == n {
        w.second_order_prefix();
        w.suffix();
        return w.s;
    }
    let n = w.s.len();
    w.suffix();
    if w.s.len() != n {
        w.second_order_prefix();
    }
    w.s
}

impl State {
    fn cut(&mut self, n: usize, repl: &str, flag: u32) {
        self.s = format!("{repl}{}", &self.s[n..]);
        self.syllables -= 1;
        self.flags |= flag;
    }

    fn trim(&mut self, n: usize) {
        self.s.truncate(self.s.len() - n);
        self.syllables -= 1;
    }

    fn particle(&mut self) {
        if self.syllables > 2
            && (self.s.ends_with("kah") || self.s.ends_with("lah") || self.s.ends_with("pun"))
        {
            self.trim(3);
        }
    }

    fn possessive(&mut self) {
        if self.syllables <= 2 {
            return;
        }
        if self.s.ends_with("ku") || self.s.ends_with("mu") {
            self.trim(2);
        } else if self.s.ends_with("nya") {
            self.trim(3);
        }
    }

    fn first_order_prefix(&mut self) {
        if self.syllables <= 2 {
            return;
        }
        let s = self.s.as_bytes();
        let vowel_at = |i: usize| s.len() > i && is_vowel(s[i]);
        if s.starts_with(b"meng") {
            self.cut(4, "", REMOVED_MENG);
        } else if s.starts_with(b"meny") && vowel_at(4) {
            self.cut(4, "s", REMOVED_MENG);
        } else if s.starts_with(b"men") || s.starts_with(b"mem") {
            self.cut(3, "", REMOVED_MENG);
        } else if s.starts_with(b"me") {
            self.cut(2, "", REMOVED_MENG);
        } else if s.starts_with(b"peng") {
            self.cut(4, "", REMOVED_PENG);
        } else if s.starts_with(b"peny") && vowel_at(4) {
            self.cut(4, "s", REMOVED_PENG);
        } else if s.starts_with(b"peny") {
            self.cut(4, "", REMOVED_PENG);
        } else if s.starts_with(b"pen") && vowel_at(3) {
            self.cut(3, "t", REMOVED_PENG);
        } else if s.starts_with(b"pen") || s.starts_with(b"pem") {
            self.cut(3, "", REMOVED_PENG);
        } else if s.starts_with(b"di") {
            self.cut(2, "", REMOVED_DI);
        } else if s.starts_with(b"ter") {
            self.cut(3, "", REMOVED_TER);
        } else if s.starts_with(b"ke") {
            self.cut(2, "", REMOVED_KE);
        }
    }

    fn second_order_prefix(&mut self) {
        if self.syllables <= 2 {
            return;
        }
        let s = self.s.as_bytes();
        if s.starts_with(b"ber") || s == b"belajar" {
            self.cut(3, "", REMOVED_BER);
        } else if s.len() > 4 && s.starts_with(b"be") && !is_vowel(s[2]) && &s[3..5] == b"er" {
            self.cut(2, "", REMOVED_BER);
        } else if s.starts_with(b"per") || s == b"pelajar" {
            self.cut(3, "", 0);
        } else if s.starts_with(b"pe") {
            self.cut(2, "", REMOVED_PE);
        }
    }

    fn suffix(&mut self) {
        if self.syllables <= 2 {
            return;
        }
        let s = &self.s;
        if s.ends_with("kan") && self.flags & (REMOVED_KE | REMOVED_PENG | REMOVED_PE) == 0 {
            self.trim(3);
        } else if s.ends_with("an") && self.flags & (REMOVED_DI | REMOVED_MENG | REMOVED_TER) == 0 {
            self.trim(2);
        } else if s.ends_with('i')
            && !s.ends_with("si")
            && self.flags & (REMOVED_BER | REMOVED_KE | REMOVED_PENG) == 0
        {
            self.trim(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::stem;

    #[test]
    fn stems() {
        for (input, want) in [
            ("pembelajaran", "ajar"),
            ("belajar", "ajar"),
            ("perkantoran", "kantor"),
            ("beragama", "agama"),
            ("pembatalan", "batal"),
            ("perkawinan", "kawin"),
            ("penulisan", "tulis"),
            ("menyapu", "sapu"),
            ("memasarkan", "asar"),
            ("pemasaran", "asar"),
            ("menganalisis", "analisis"),
            ("bukunya", "buku"),
            ("bacalah", "baca"),
            ("malang", "malang"),
            ("2017", "2017"),
        ] {
            assert_eq!(stem(input), want, "stem({input:?})");
        }
    }
}
