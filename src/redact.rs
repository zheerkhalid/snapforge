//! Finds private details in recognized text (e-mail addresses, phone numbers, payment
//! card numbers) so the editor can cover them. Pure: it works on an [`OcrResult`] and
//! returns rectangles in image pixels; nothing here touches pixels or the network.
//!
//! The matchers are deliberately conservative. A miss is visible to the user, who can
//! cover it by hand; a screenshot full of wrongly covered numbers is just annoying.

use crate::ocr::{BoxF, OcrLine, OcrResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Email,
    Phone,
    Card,
}

/// Which kinds to look for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kinds {
    pub email: bool,
    pub phone: bool,
    pub card: bool,
}

impl Kinds {
    #[cfg(test)]
    pub const ALL: Kinds = Kinds { email: true, phone: true, card: true };

    pub fn any(self) -> bool {
        self.email || self.phone || self.card
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub kind: Kind,
    /// Area to cover, in image pixels (already padded a little).
    pub rect: BoxF,
}

/// Per-kind totals of `hits`: (e-mails, phones, cards).
pub fn counts(hits: &[Hit]) -> (usize, usize, usize) {
    let n = |k: Kind| hits.iter().filter(|h| h.kind == k).count();
    (n(Kind::Email), n(Kind::Phone), n(Kind::Card))
}

/// Every private detail of the wanted kinds in `ocr`, top to bottom.
pub fn find(ocr: &OcrResult, kinds: Kinds) -> Vec<Hit> {
    let mut hits = Vec::new();
    if !kinds.any() {
        return hits;
    }
    for line in &ocr.lines {
        let lt = LineText::new(line);
        for (kind, start, end) in find_in_text(&lt.chars, kinds) {
            if let Some(rect) = lt.rect_of(line, start, end) {
                hits.push(Hit { kind, rect });
            }
        }
    }
    hits
}

/// Matches in one line of text as (kind, start, end) character ranges, without overlaps
/// (cards win over phones, which win over nothing; e-mails never overlap numbers).
pub fn find_in_text(chars: &[char], kinds: Kinds) -> Vec<(Kind, usize, usize)> {
    let mut out: Vec<(Kind, usize, usize)> = Vec::new();
    if kinds.email {
        out.extend(emails(chars).into_iter().map(|(s, e)| (Kind::Email, s, e)));
    }
    // Cards are always located, so a card number is never reported as a phone number.
    let cards = cards(chars);
    let free = |out: &[(Kind, usize, usize)], s: usize, e: usize| out.iter().all(|&(_, a, b)| e <= a || s >= b);
    for &(s, e) in &cards {
        if kinds.card && free(&out, s, e) {
            out.push((Kind::Card, s, e));
        }
    }
    if kinds.phone {
        for (s, e) in phones(chars) {
            let on_card = cards.iter().any(|&(a, b)| s < b && e > a);
            if !on_card && free(&out, s, e) {
                out.push((Kind::Phone, s, e));
            }
        }
    }
    out.sort_by_key(|&(_, s, _)| s);
    out
}

// ------------------------------------------------------------------ line text

/// A character that never appears inside a match: marks a wide gap between two words
/// (table cells, columns), so numbers in neighbouring cells are not read as one number.
const GAP: char = '\t';

/// The line as characters, with the word each character came from.
struct LineText {
    chars: Vec<char>,
    /// Word index per character (`usize::MAX` for the separators between words).
    word: Vec<usize>,
    /// First character of each word.
    start: Vec<usize>,
}

impl LineText {
    fn new(line: &OcrLine) -> Self {
        let mut t = LineText { chars: Vec::new(), word: Vec::new(), start: Vec::new() };
        for (i, w) in line.words.iter().enumerate() {
            if i > 0 {
                let prev = &line.words[i - 1].rect;
                let gap = w.rect.x - (prev.x + prev.w);
                let wide = gap > prev.h.max(w.rect.h) * 1.2;
                t.chars.push(if wide { GAP } else { ' ' });
                t.word.push(usize::MAX);
            }
            t.start.push(t.chars.len());
            for c in w.text.chars() {
                t.chars.push(c);
                t.word.push(i);
            }
        }
        t
    }

    /// The box around characters `start..end`: whole words, except that the first and
    /// last word are trimmed in proportion when the match covers only part of them
    /// ("Email:name@example.com," is one OCR word).
    fn rect_of(&self, line: &OcrLine, start: usize, end: usize) -> Option<BoxF> {
        let mut acc: Option<BoxF> = None;
        let mut i = start;
        while i < end {
            let wi = self.word[i];
            if wi == usize::MAX {
                i += 1;
                continue;
            }
            let w = &line.words[wi];
            let w_start = self.start[wi];
            let len = w.text.chars().count().max(1);
            let from = i - w_start;
            let to = (end - w_start).min(len);
            let part = BoxF {
                x: w.rect.x + w.rect.w * from as f32 / len as f32,
                y: w.rect.y,
                w: w.rect.w * (to - from) as f32 / len as f32,
                h: w.rect.h,
            };
            acc = Some(acc.map_or(part, |a| a.union(&part)));
            i = w_start + len;
        }
        // A little padding: OCR boxes hug the glyphs, and partial trims are estimates.
        acc.map(|r| {
            let pad = (r.h * 0.18).max(2.0);
            BoxF { x: r.x - pad, y: r.y - pad, w: r.w + 2.0 * pad, h: r.h + 2.0 * pad }
        })
    }
}

// ------------------------------------------------------------------ e-mail

fn is_local(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-')
}

fn is_domain(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '-')
}

fn emails(c: &[char]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for at in 0..c.len() {
        if c[at] != '@' {
            continue;
        }
        // OCR sometimes puts a space on one side of the @.
        let mut l = at;
        if l > 0 && c[l - 1] == ' ' {
            l -= 1;
        }
        let local_end = l;
        while l > 0 && is_local(c[l - 1]) {
            l -= 1;
        }
        while l < local_end && c[l] == '.' {
            l += 1;
        }
        let mut r = at + 1;
        if r < c.len() && c[r] == ' ' {
            r += 1;
        }
        let dom_start = r;
        while r < c.len() && is_domain(c[r]) {
            r += 1;
        }
        while r > dom_start && matches!(c[r - 1], '.' | '-') {
            r -= 1;
        }
        let domain = &c[dom_start..r];
        let tld = domain.iter().rposition(|&ch| ch == '.').map(|i| &domain[i + 1..]);
        let tld_ok = tld.is_some_and(|t| t.len() >= 2 && t.iter().all(|ch| ch.is_ascii_alphabetic()));
        if l < local_end && tld_ok && domain[0] != '.' {
            out.push((l, r));
        }
    }
    out
}

// ------------------------------------------------------------------ numbers

/// A run of digit groups: `groups` are (start, end) character ranges of consecutive
/// digits, joined by one or two separator characters from `seps`.
fn digit_runs(c: &[char], seps: &[char]) -> Vec<Vec<(usize, usize)>> {
    let mut runs = Vec::new();
    let mut i = 0;
    while i < c.len() {
        if !c[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let mut groups = Vec::new();
        loop {
            let s = i;
            while i < c.len() && c[i].is_ascii_digit() {
                i += 1;
            }
            groups.push((s, i));
            // Up to two separators (") " or " (") may sit between groups.
            let mut j = i;
            while j < c.len() && j - i < 2 && seps.contains(&c[j]) {
                j += 1;
            }
            if j > i && j < c.len() && c[j].is_ascii_digit() {
                i = j;
            } else {
                break;
            }
        }
        runs.push(groups);
    }
    runs
}

fn digits_in(c: &[char], groups: &[(usize, usize)]) -> Vec<u8> {
    groups.iter().flat_map(|&(s, e)| c[s..e].iter().map(|ch| *ch as u8 - b'0')).collect()
}

fn luhn(digits: &[u8]) -> bool {
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &d)| {
            let d = d as u32;
            if i % 2 == 1 {
                let x = d * 2;
                if x > 9 {
                    x - 9
                } else {
                    x
                }
            } else {
                d
            }
        })
        .sum();
    sum % 10 == 0
}

/// A letter or digit right before or after the number means it is part of something else
/// (an order id like "AB4111…", a hash).
fn glued(c: &[char], s: usize, e: usize) -> bool {
    (s > 0 && c[s - 1].is_alphanumeric()) || (e < c.len() && c[e].is_alphanumeric())
}

fn cards(c: &[char]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for groups in digit_runs(c, &[' ', '-']) {
        // The longest stretch of whole groups that is a valid card number; a trailing
        // security code or a leading label number is left out.
        let mut best: Option<(usize, usize)> = None;
        'search: for a in 0..groups.len() {
            for b in (a..groups.len()).rev() {
                let d = digits_in(c, &groups[a..=b]);
                // Not all the same digit ("0000 0000 0000 0000" passes the checksum).
                let varied = d.iter().any(|&x| x != d[0]);
                if (13..=19).contains(&d.len()) && varied && luhn(&d) {
                    best = Some((groups[a].0, groups[b].1));
                    break 'search;
                }
            }
        }
        if let Some((s, e)) = best {
            // "0.4111…": the digits after a decimal point are not a card number.
            let decimal = s >= 2 && matches!(c[s - 1], '.' | ',') && c[s - 2].is_ascii_digit();
            if !glued(c, s, e) && !decimal {
                out.push((s, e));
            }
        }
    }
    out
}

fn phones(c: &[char]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for groups in digit_runs(c, &[' ', '-', '.', '(', ')', '/']) {
        let (first, last) = (groups[0], groups[groups.len() - 1]);
        let mut s = first.0;
        let e = last.1;
        // A leading "(" and "+" belong to the number.
        if s > 0 && c[s - 1] == '(' {
            s -= 1;
        }
        let plus = s > 0 && c[s - 1] == '+';
        if plus {
            s -= 1;
        }
        if glued(c, s, e) {
            continue;
        }
        let lens: Vec<usize> = groups.iter().map(|&(a, b)| b - a).collect();
        let n: usize = lens.iter().sum();
        if !(7..=15).contains(&n) {
            continue;
        }
        let seps: Vec<char> =
            groups.windows(2).flat_map(|w| c[w[0].1..w[1].0].iter().copied()).filter(|ch| *ch != ' ').collect();
        let only = |ch: char| !seps.is_empty() && seps.iter().all(|&x| x == ch);
        let ok = if plus {
            // International form: "+44 20 7946 0958", "+15551234567".
            n >= 8
        } else if groups.len() == 1 {
            // A bare number is a phone only with a trunk zero ("07700900123").
            (10..=13).contains(&n) && c[first.0] == '0'
        } else if only('/') {
            // "24/11/2026" and "1/2/3" are dates and fractions; "030/1234567" is a phone.
            groups.len() == 2 && n >= 9 && c[first.0] == '0'
        } else if only('.')
            && (groups.len() < 3
                || (groups.len() == 4 && lens.iter().all(|&l| l <= 3))
                || lens[1..].iter().all(|&l| l == 3))
        {
            // Decimals ("1234567.89"), IPv4 addresses and numbers with thousands
            // separators ("1.234.567.890").
            false
        } else if groups.len() == 2 && lens == [3, 4] && only('-') {
            // Local seven-digit form "555-0123".
            true
        } else {
            // "(555) 123-4567", "020 7946 0958", "06 12 34 56 78", "555.123.4567":
            // at least nine digits in short groups (long groups are ids or timestamps).
            n >= 9 && lens.iter().all(|&l| l <= 7) && !looks_like_date(&lens)
        };
        if ok {
            out.push((s, e));
        }
    }
    out
}

/// Digit groups shaped like a date with a time ("2026-11-24 10 30", "24.11.2026 1030").
fn looks_like_date(lens: &[usize]) -> bool {
    lens.len() >= 3 && (lens[..3] == [4, 2, 2] || lens[..3] == [2, 2, 4])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr::OcrWord;

    fn found(text: &str) -> Vec<(Kind, String)> {
        let chars: Vec<char> = text.chars().collect();
        find_in_text(&chars, Kinds::ALL).into_iter().map(|(k, s, e)| (k, chars[s..e].iter().collect())).collect()
    }

    fn one(text: &str) -> Option<(Kind, String)> {
        let f = found(text);
        assert!(f.len() <= 1, "{text:?} → {f:?}");
        f.into_iter().next()
    }

    #[test]
    fn emails_are_found_with_ocr_quirks() {
        assert_eq!(
            one("Contact: jane.doe+news@mail.example.org today"),
            Some((Kind::Email, "jane.doe+news@mail.example.org".into()))
        );
        assert_eq!(one("Email:first_last@example.com,"), Some((Kind::Email, "first_last@example.com".into())));
        // A space next to the @ (split word) and a full stop that ends the sentence.
        assert_eq!(one("write to info @example.co.uk."), Some((Kind::Email, "info @example.co.uk".into())));
        assert_eq!(one("a@b.io"), Some((Kind::Email, "a@b.io".into())));
        for no in ["@handle", "user@localhost", "5 @ 3", "price @ 4.50", "name@domain.c", "user@.com", "@", "x@y.123"] {
            assert_eq!(one(no), None, "{no}");
        }
        assert_eq!(found("a@b.io c@d.org").len(), 2);
    }

    #[test]
    fn card_numbers_need_the_checksum() {
        // Well-known test numbers (not real cards).
        assert_eq!(one("Card 4111 1111 1111 1111 exp 12/27"), Some((Kind::Card, "4111 1111 1111 1111".into())));
        assert_eq!(one("4111-1111-1111-1111"), Some((Kind::Card, "4111-1111-1111-1111".into())));
        assert_eq!(one("5500005555555559"), Some((Kind::Card, "5500005555555559".into())));
        assert_eq!(one("Amex 3782 822463 10005"), Some((Kind::Card, "3782 822463 10005".into())));
        // A security code after the number is left out of the match.
        assert_eq!(one("4111 1111 1111 1111 123"), Some((Kind::Card, "4111 1111 1111 1111".into())));
        // Wrong checksum, all zeros, or glued to letters: not a card.
        assert_eq!(one("4111 1111 1111 1112 x"), None);
        assert_eq!(one("0000 0000 0000 0000"), None);
        assert_eq!(one("ID4111111111111111"), None);
        assert_eq!(one("order 4111111111111111a"), None);
    }

    #[test]
    fn phone_numbers_in_common_shapes() {
        for (text, want) in [
            ("Call +44 20 7946 0958 now", "+44 20 7946 0958"),
            ("+15550100123", "+15550100123"),
            ("tel (555) 010-4567.", "(555) 010-4567"),
            ("555.010.4567", "555.010.4567"),
            ("020 7946 0958", "020 7946 0958"),
            ("06 12 34 56 78", "06 12 34 56 78"),
            ("Mobile: 07700900123", "07700900123"),
            ("555-0123", "555-0123"),
            ("030/1234567", "030/1234567"),
            ("+964 750 123 4567", "+964 750 123 4567"),
        ] {
            assert_eq!(one(text), Some((Kind::Phone, want.into())), "{text}");
        }
    }

    #[test]
    fn ordinary_numbers_are_left_alone() {
        for no in [
            "2026-11-24",
            "24/11/2026",
            "24.11.2026",
            "2026-11-24 10 30",
            "192.168.100.200",
            "1.234.567.890",
            "1234567.89",
            "Total 1 234 567",
            "1920 x 1080",
            "Order 1732406400",
            "20261124 103000",
            "v1.2.3",
            "page 12 of 340",
            "3.14159265358979",
            "#4521",
            "2019-2024",
            "A1234567890",
            "100% 200% 300%",
        ] {
            assert_eq!(found(no), [], "{no}");
        }
        // Cells that sit far apart in a table are separated by a gap marker.
        let cells = format!("100{GAP}200{GAP}300{GAP}400");
        assert_eq!(found(&cells), []);
    }

    #[test]
    fn kinds_filter_and_cards_never_become_phones() {
        let chars: Vec<char> = "a@b.io 4111 1111 1111 1111 +44 20 7946 0958".chars().collect();
        let only = |k: Kinds| find_in_text(&chars, k).into_iter().map(|(k, ..)| k).collect::<Vec<_>>();
        assert_eq!(only(Kinds::ALL), [Kind::Email, Kind::Card, Kind::Phone]);
        assert_eq!(only(Kinds { email: true, phone: false, card: false }), [Kind::Email]);
        // With cards off, the card number is not reported as a phone number either.
        assert_eq!(only(Kinds { email: false, phone: true, card: false }), [Kind::Phone]);
        assert_eq!(only(Kinds { email: false, phone: false, card: false }), []);
        assert!(!Kinds { email: false, phone: false, card: false }.any());
    }

    #[test]
    fn luhn_checksum() {
        let d = |s: &str| s.bytes().map(|b| b - b'0').collect::<Vec<u8>>();
        assert!(luhn(&d("4111111111111111")));
        assert!(luhn(&d("79927398713")));
        assert!(!luhn(&d("79927398710")));
    }

    fn word(text: &str, x: f32, w: f32) -> OcrWord {
        OcrWord { text: text.into(), rect: BoxF { x, y: 100.0, w, h: 20.0 } }
    }

    #[test]
    fn hits_map_back_to_word_boxes() {
        let line = OcrLine {
            words: vec![
                word("Mail", 10.0, 40.0),
                word("jane@example.com", 56.0, 160.0),
                word("or", 222.0, 20.0),
                word("+44", 248.0, 30.0),
                word("20", 284.0, 20.0),
                word("7946", 310.0, 40.0),
                word("0958", 356.0, 40.0),
            ],
        };
        let ocr = OcrResult { lines: vec![line], language: "en-US".into(), angle: None };
        let hits = find(&ocr, Kinds::ALL);
        assert_eq!(counts(&hits), (1, 1, 0));
        let pad = 20.0 * 0.18;
        let mail = &hits[0];
        assert_eq!(mail.kind, Kind::Email);
        assert!((mail.rect.x - (56.0 - pad)).abs() < 0.01 && (mail.rect.w - (160.0 + 2.0 * pad)).abs() < 0.01);
        assert!((mail.rect.y - (100.0 - pad)).abs() < 0.01 && (mail.rect.h - (20.0 + 2.0 * pad)).abs() < 0.01);
        // The phone number spans four words.
        let phone = &hits[1];
        assert!((phone.rect.x - (248.0 - pad)).abs() < 0.01, "{phone:?}");
        assert!((phone.rect.x + phone.rect.w - (396.0 + pad)).abs() < 0.01, "{phone:?}");
        assert!(find(&ocr, Kinds { email: false, phone: false, card: false }).is_empty());
    }

    #[test]
    fn partial_words_are_trimmed_in_proportion() {
        // "Email:ab@cd.io," is one 15-character word, 150 px wide: the address is
        // characters 6..14.
        let line = OcrLine { words: vec![word("Email:ab@cd.io,", 0.0, 150.0)] };
        let ocr = OcrResult { lines: vec![line], language: "en-US".into(), angle: None };
        let hits = find(&ocr, Kinds::ALL);
        assert_eq!(hits.len(), 1);
        let pad = 20.0 * 0.18;
        assert!((hits[0].rect.x - (60.0 - pad)).abs() < 0.01, "{:?}", hits[0]);
        assert!((hits[0].rect.w - (80.0 + 2.0 * pad)).abs() < 0.01, "{:?}", hits[0]);
    }

    #[test]
    fn wide_gaps_split_numbers_in_table_cells() {
        // Three cells 60 px apart (three times the text height) must not join into a
        // phone number; the same digits typed with normal spaces do.
        let cells = OcrLine { words: vec![word("555", 0.0, 30.0), word("010", 90.0, 30.0), word("4567", 180.0, 40.0)] };
        let typed = OcrLine { words: vec![word("555", 0.0, 30.0), word("010", 36.0, 30.0), word("4567", 72.0, 40.0)] };
        let ocr = |l: OcrLine| OcrResult { lines: vec![l], language: "en-US".into(), angle: None };
        assert!(find(&ocr(cells), Kinds::ALL).is_empty());
        assert_eq!(counts(&find(&ocr(typed), Kinds::ALL)), (0, 1, 0));
    }
}
