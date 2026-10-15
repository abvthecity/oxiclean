//! Source positions in a trace, made readable: `tsc` records each span's
//! start before its leading trivia, and analyze-trace moves it forward to
//! the first character of code and turns offsets into lines and columns.
//! It also counts the `import("…")` types a declaration file repeats.
//!
//! Both run on a small state machine that tells code from comments,
//! strings and whitespace without parsing, ported from analyze-trace's
//! `trivia-state-machine.ts` so positions match it. Offsets and columns
//! count UTF-16 code units, as `tsc`'s do.

use std::collections::HashMap;

/// What a character is part of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharKind {
    Comment,
    String,
    Regex,
    Whitespace,
    Code,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Default,
    StartSingleLineComment,
    SingleLineComment,
    StartMultiLineComment,
    MultiLineComment,
    EndMultiLineComment,
    StartShebangComment,
    ShebangComment,
    SingleQuoteString,
    SingleQuoteStringEscapeBackslash,
    SingleQuoteStringEscapeQuote,
    DoubleQuoteString,
    DoubleQuoteStringEscapeBackslash,
    DoubleQuoteStringEscapeQuote,
    TemplateString,
    TemplateStringEscapeBackslash,
    TemplateStringEscapeQuote,
    StartExpressionHole,
    Regex,
    RegexEscapeBackslash,
    RegexEscapeSlash,
    RegexEscapeOpenBracket,
    CharClass,
    CharClassEscapeBackslash,
    CharClassEscapeCloseBracket,
}

const CR: u16 = b'\r' as u16;
const LF: u16 = b'\n' as u16;
const SLASH: u16 = b'/' as u16;
const BACKSLASH: u16 = b'\\' as u16;
const STAR: u16 = b'*' as u16;
const HASH: u16 = b'#' as u16;
const BANG: u16 = b'!' as u16;
const SINGLE_QUOTE: u16 = b'\'' as u16;
const DOUBLE_QUOTE: u16 = b'"' as u16;
const OPEN_BRACE: u16 = b'{' as u16;
const CLOSE_BRACE: u16 = b'}' as u16;
const OPEN_BRACKET: u16 = b'[' as u16;
const CLOSE_BRACKET: u16 = b']' as u16;
const BACKTICK: u16 = b'`' as u16;
const DOLLAR: u16 = b'$' as u16;

/// Whether JavaScript's `\s` matches `ch`.
fn is_js_whitespace(ch: u16) -> bool {
    matches!(
        ch,
        0x09..=0x0d
            | 0x20
            | 0xa0
            | 0x1680
            | 0x2000..=0x200a
            | 0x2028
            | 0x2029
            | 0x202f
            | 0x205f
            | 0x3000
            | 0xfeff
    )
}

/// The state machine, fed one UTF-16 code unit at a time with the next.
struct Machine {
    state: State,
    brace_depth: usize,
    template_brace_depths: Vec<usize>,
    at_start: bool,
}

impl Machine {
    fn new() -> Self {
        Self {
            state: State::Default,
            brace_depth: 0,
            template_brace_depths: Vec::new(),
            at_start: true,
        }
    }

    /// What `ch` is part of, and whether it ends a line. `next` is the code
    /// unit after it, if any.
    #[allow(clippy::too_many_lines)]
    fn step(&mut self, ch: u16, next: Option<u16>) -> (CharKind, bool) {
        use State as S;
        let mut next_state = None;
        let mut wrap_line = false;
        match ch {
            CR if next == Some(LF) => {
                if matches!(
                    self.state,
                    S::ShebangComment
                        | S::SingleLineComment
                        | S::SingleQuoteString
                        | S::DoubleQuoteString
                        | S::Regex
                        | S::CharClass
                ) {
                    self.state = S::Default;
                }
            }
            CR | LF => {
                wrap_line = true;
                if matches!(
                    self.state,
                    S::ShebangComment
                        | S::SingleLineComment
                        | S::SingleQuoteString
                        | S::DoubleQuoteString
                ) {
                    self.state = S::Default;
                }
            }
            SLASH => match self.state {
                S::Default => {
                    self.state = match next {
                        Some(SLASH) => S::StartSingleLineComment,
                        Some(STAR) => S::StartMultiLineComment,
                        // As in analyze-trace, a division reads as a regex.
                        _ => S::Regex,
                    };
                }
                S::StartSingleLineComment => self.state = S::SingleLineComment,
                S::EndMultiLineComment | S::Regex => next_state = Some(S::Default),
                S::RegexEscapeSlash => next_state = Some(S::Regex),
                _ => {}
            },
            STAR => match self.state {
                S::StartMultiLineComment => self.state = S::MultiLineComment,
                S::MultiLineComment if next == Some(SLASH) => {
                    self.state = S::EndMultiLineComment;
                }
                _ => {}
            },
            HASH => {
                if self.at_start && self.state == S::Default && next == Some(BANG) {
                    self.state = S::StartShebangComment;
                }
            }
            BANG => {
                if self.state == S::StartShebangComment {
                    self.state = S::ShebangComment;
                }
            }
            SINGLE_QUOTE => match self.state {
                S::Default => self.state = S::SingleQuoteString,
                S::SingleQuoteStringEscapeQuote => next_state = Some(S::SingleQuoteString),
                S::SingleQuoteString => next_state = Some(S::Default),
                _ => {}
            },
            DOUBLE_QUOTE => match self.state {
                S::Default => self.state = S::DoubleQuoteString,
                S::DoubleQuoteStringEscapeQuote => next_state = Some(S::DoubleQuoteString),
                S::DoubleQuoteString => next_state = Some(S::Default),
                _ => {}
            },
            BACKTICK => match self.state {
                S::Default => self.state = S::TemplateString,
                S::TemplateStringEscapeQuote => next_state = Some(S::TemplateString),
                S::TemplateString => next_state = Some(S::Default),
                _ => {}
            },
            BACKSLASH => match self.state {
                S::SingleQuoteString => match next {
                    Some(SINGLE_QUOTE) => self.state = S::SingleQuoteStringEscapeQuote,
                    Some(BACKSLASH) => self.state = S::SingleQuoteStringEscapeBackslash,
                    _ => {}
                },
                S::DoubleQuoteString => match next {
                    Some(DOUBLE_QUOTE) => self.state = S::DoubleQuoteStringEscapeQuote,
                    Some(BACKSLASH) => self.state = S::DoubleQuoteStringEscapeBackslash,
                    _ => {}
                },
                S::TemplateString => match next {
                    Some(BACKTICK) => self.state = S::TemplateStringEscapeQuote,
                    Some(BACKSLASH) => self.state = S::TemplateStringEscapeBackslash,
                    _ => {}
                },
                S::Regex => match next {
                    Some(OPEN_BRACKET) => self.state = S::RegexEscapeOpenBracket,
                    Some(SLASH) => self.state = S::RegexEscapeSlash,
                    Some(BACKSLASH) => self.state = S::RegexEscapeBackslash,
                    _ => {}
                },
                S::CharClass => match next {
                    Some(CLOSE_BRACKET) => self.state = S::CharClassEscapeCloseBracket,
                    Some(BACKSLASH) => self.state = S::CharClassEscapeBackslash,
                    _ => {}
                },
                S::SingleQuoteStringEscapeBackslash => next_state = Some(S::SingleQuoteString),
                S::DoubleQuoteStringEscapeBackslash => next_state = Some(S::DoubleQuoteString),
                S::TemplateStringEscapeBackslash => next_state = Some(S::TemplateString),
                S::RegexEscapeBackslash => next_state = Some(S::Regex),
                S::CharClassEscapeBackslash => next_state = Some(S::CharClass),
                _ => {}
            },
            DOLLAR => {
                if self.state == S::TemplateString && next == Some(OPEN_BRACE) {
                    self.state = S::StartExpressionHole;
                }
            }
            OPEN_BRACE => match self.state {
                S::Default => self.brace_depth += 1,
                S::StartExpressionHole => {
                    self.template_brace_depths.push(self.brace_depth);
                    next_state = Some(S::Default);
                }
                _ => {}
            },
            CLOSE_BRACE => {
                if self.template_brace_depths.last() == Some(&self.brace_depth) {
                    self.template_brace_depths.pop();
                    self.state = S::TemplateString;
                } else if self.state == S::Default && self.brace_depth > 0 {
                    self.brace_depth -= 1;
                }
            }
            OPEN_BRACKET => match self.state {
                S::RegexEscapeOpenBracket => next_state = Some(S::Regex),
                S::Regex => self.state = S::CharClass,
                _ => {}
            },
            CLOSE_BRACKET => match self.state {
                S::CharClassEscapeCloseBracket => next_state = Some(S::CharClass),
                S::CharClass => next_state = Some(S::Regex),
                _ => {}
            },
            _ => {}
        }
        let kind = match self.state {
            S::StartSingleLineComment
            | S::SingleLineComment
            | S::StartMultiLineComment
            | S::MultiLineComment
            | S::EndMultiLineComment
            | S::StartShebangComment
            | S::ShebangComment => CharKind::Comment,
            S::SingleQuoteString
            | S::SingleQuoteStringEscapeBackslash
            | S::SingleQuoteStringEscapeQuote
            | S::DoubleQuoteString
            | S::DoubleQuoteStringEscapeBackslash
            | S::DoubleQuoteStringEscapeQuote
            | S::TemplateString
            | S::TemplateStringEscapeBackslash
            | S::TemplateStringEscapeQuote
            | S::StartExpressionHole => CharKind::String,
            S::Regex
            | S::RegexEscapeBackslash
            | S::RegexEscapeSlash
            | S::RegexEscapeOpenBracket
            | S::CharClass
            | S::CharClassEscapeBackslash
            | S::CharClassEscapeCloseBracket => CharKind::Regex,
            S::Default if is_js_whitespace(ch) => CharKind::Whitespace,
            S::Default => CharKind::Code,
        };
        if let Some(s) = next_state {
            self.state = s;
        }
        self.at_start = false;
        (kind, wrap_line)
    }
}

/// Runs the machine over `text`, calling `f` with each code unit, its
/// offset, its kind and whether it ends a line.
fn scan(text: &str, mut f: impl FnMut(u16, usize, CharKind, bool)) {
    let mut machine = Machine::new();
    let mut units = text.encode_utf16().peekable();
    let mut offset = 0;
    while let Some(ch) = units.next() {
        let (kind, wrap) = machine.step(ch, units.peek().copied());
        f(ch, offset, kind, wrap);
        offset += 1;
    }
}

/// A position in a trace to make readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum RawPosition {
    /// A UTF-16 offset, moved past trivia to the next code: a span's start.
    Start(u64),
    /// A UTF-16 offset left where it is: a span's end.
    End(u64),
    /// A 1-based line and column, moved past trivia to the next code: where
    /// a type is declared, as `types.json` gives it.
    LineChar(u64, u64),
}

/// A 1-based line and column, in UTF-16 code units.
pub(crate) type LineChar = (u64, u64);

/// Each of `positions` as a line and column in `text`, as analyze-trace's
/// `normalizePositions` makes them: starts move forward past whitespace
/// and comments; anything past the end is the end of the file.
pub(crate) fn normalize(text: &str, positions: &[RawPosition]) -> HashMap<RawPosition, LineChar> {
    // Sorted, per kind, by where each lands; each list is walked once.
    let mut fixed: Vec<u64> = Vec::new();
    let mut skip: Vec<u64> = Vec::new();
    let mut line_chars: Vec<LineChar> = Vec::new();
    for p in positions {
        match *p {
            RawPosition::End(o) => fixed.push(o),
            RawPosition::Start(o) => skip.push(o),
            RawPosition::LineChar(l, c) => line_chars.push((l, c)),
        }
    }
    fixed.sort_unstable();
    fixed.dedup();
    skip.sort_unstable();
    skip.dedup();
    line_chars.sort_unstable();
    line_chars.dedup();

    let mut out = HashMap::with_capacity(positions.len());
    let (mut fi, mut si, mut li) = (0, 0, 0);
    let mut current: LineChar = (1, 1);
    scan(text, |_, offset, kind, wrap| {
        let offset = offset as u64;
        while fi < fixed.len() && fixed[fi] <= offset {
            out.insert(RawPosition::End(fixed[fi]), current);
            fi += 1;
        }
        if !matches!(kind, CharKind::Comment | CharKind::Whitespace) {
            while si < skip.len() && skip[si] <= offset {
                out.insert(RawPosition::Start(skip[si]), current);
                si += 1;
            }
            while li < line_chars.len() && line_chars[li] <= current {
                let (l, c) = line_chars[li];
                out.insert(RawPosition::LineChar(l, c), current);
                li += 1;
            }
        }
        if wrap {
            current = (current.0 + 1, 1);
        } else {
            current.1 += 1;
        }
    });
    for &o in &fixed[fi..] {
        out.insert(RawPosition::End(o), current);
    }
    for &o in &skip[si..] {
        out.insert(RawPosition::Start(o), current);
    }
    for &(l, c) in &line_chars[li..] {
        out.insert(RawPosition::LineChar(l, c), current);
    }
    out
}

/// How often each module a declaration file names in `import("…")` types
/// is named, as analyze-trace counts them. Each name keeps its quotes.
pub(crate) fn count_import_types(text: &str) -> HashMap<String, usize> {
    const TARGET: &[u8] = b"import(";
    let mut frequency: HashMap<String, usize> = HashMap::new();
    let mut matched = 0;
    let mut name: Vec<u16> = Vec::new();
    scan(text, |ch, _, kind, _| {
        if matched == TARGET.len() {
            if kind == CharKind::String {
                name.push(ch);
            } else {
                *frequency.entry(String::from_utf16_lossy(&name)).or_default() += 1;
                matched = 0;
                name.clear();
            }
        } else if kind == CharKind::Code && ch == u16::from(TARGET[matched]) {
            matched += 1;
        } else {
            matched = 0;
        }
    });
    frequency
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str, p: RawPosition) -> LineChar {
        normalize(text, &[p])[&p]
    }

    #[test]
    fn starts_move_past_trivia_and_ends_stay() {
        let text = "const a = 1;\n// note\n  const b = 2;\n";
        // `tsc` starts the second statement right after the first `;`.
        assert_eq!(at(text, RawPosition::Start(12)), (3, 3));
        assert_eq!(at(text, RawPosition::End(12)), (1, 13));
        assert_eq!(at(text, RawPosition::Start(0)), (1, 1));
        // Past the end: the end of the file.
        assert_eq!(at(text, RawPosition::End(999)), (4, 1));
        assert_eq!(at(text, RawPosition::LineChar(1, 13)), (3, 3));
    }

    #[test]
    fn strings_and_regexes_are_code() {
        let text = "/* c */ 'it''s' /x/";
        assert_eq!(at(text, RawPosition::Start(0)), (1, 9));
        assert_eq!(at(text, RawPosition::Start(15)), (1, 17));
    }

    #[test]
    fn columns_count_utf16_units_and_crlf_is_one_line_break() {
        let text = "const s = \"\u{1F600}\";\r\nx;";
        // The emoji is two UTF-16 units.
        assert_eq!(at(text, RawPosition::End(14)), (1, 15));
        assert_eq!(at(text, RawPosition::Start(15)), (2, 1));
        let text = "a\rb";
        assert_eq!(at(text, RawPosition::Start(2)), (2, 1));
    }

    #[test]
    fn template_holes_are_code() {
        let text = "`a${ {b:1}.b }c` + d";
        assert_eq!(at(text, RawPosition::Start(17)), (1, 18));
        assert_eq!(at(text, RawPosition::Start(19)), (1, 20));
    }

    #[test]
    fn counts_import_types() {
        let text = "export declare const a: import(\"react\").FC;\n// import(\"x\")\nexport declare const b: import('react').FC<import(\"./c\").P>;";
        let counts = count_import_types(text);
        assert_eq!(counts.get("\"react\""), Some(&1));
        assert_eq!(counts.get("'react'"), Some(&1));
        assert_eq!(counts.get("\"./c\""), Some(&1));
        assert_eq!(counts.get("\"x\""), None);
    }
}
