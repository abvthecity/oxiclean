//! A JSON-with-comments document that keeps where each value is, so
//! findings can point at a line and fixes can edit the text in place,
//! leaving comments and formatting alone.

use std::ops::Range;

use serde_json::Value;

/// One step of a path into a document: an object key or an array index.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Segment {
    Key(String),
    Index(usize),
}

impl From<&str> for Segment {
    fn from(key: &str) -> Self {
        Self::Key(key.to_string())
    }
}

impl From<usize> for Segment {
    fn from(index: usize) -> Self {
        Self::Index(index)
    }
}

/// A value and the bytes it spans.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Node {
    pub span: Range<usize>,
    pub kind: NodeKind,
}

/// What a [`Node`] is.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum NodeKind {
    Object(Vec<Member>),
    Array(Vec<Node>),
    /// A string, number, boolean or null; its text is the span.
    Scalar,
}

/// An object member: its key, where the key is, and its value.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Member {
    pub key: String,
    pub key_span: Range<usize>,
    pub value: Node,
}

impl Node {
    /// The member `key` of an object.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Node> {
        match &self.kind {
            NodeKind::Object(members) => members.iter().find(|m| m.key == key).map(|m| &m.value),
            _ => None,
        }
    }

    /// The node at `path` below this one.
    #[must_use]
    pub fn at(&self, path: &[Segment]) -> Option<&Node> {
        let mut node = self;
        for seg in path {
            node = match (seg, &node.kind) {
                (Segment::Key(k), NodeKind::Object(_)) => node.get(k)?,
                (Segment::Index(i), NodeKind::Array(items)) => items.get(*i)?,
                _ => return None,
            };
        }
        Some(node)
    }

    /// The items of an array, or nothing.
    #[must_use]
    pub fn items(&self) -> &[Node] {
        match &self.kind {
            NodeKind::Array(items) => items,
            _ => &[],
        }
    }

    /// The members of an object, or nothing.
    #[must_use]
    pub fn members(&self) -> &[Member] {
        match &self.kind {
            NodeKind::Object(members) => members,
            _ => &[],
        }
    }
}

/// A parsed JSONC document.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct JsoncDoc {
    pub text: String,
    pub root: Node,
}

impl JsoncDoc {
    /// Parses `text`, allowing comments and trailing commas. A leading BOM
    /// is skipped.
    #[must_use]
    pub fn parse(text: impl Into<String>) -> Option<Self> {
        let text = text.into();
        let mut p = Parser { src: text.as_bytes(), pos: 0 };
        if text.starts_with('\u{feff}') {
            p.pos = 3;
        }
        p.skip();
        let root = p.value(0)?;
        p.skip();
        (p.pos == p.src.len()).then_some(Self { text, root })
    }

    /// The node at `path`.
    #[must_use]
    pub fn at(&self, path: &[Segment]) -> Option<&Node> {
        self.root.at(path)
    }

    /// `node`'s text as a JSON value.
    #[must_use]
    pub fn value(&self, node: &Node) -> Option<Value> {
        crate::jsonc::parse_jsonc(&self.text[node.span.clone()])
    }

    /// 1-based line of byte `offset`.
    #[must_use]
    pub fn line(&self, offset: usize) -> u32 {
        let n = self.text.as_bytes()[..offset.min(self.text.len())]
            .iter()
            .filter(|&&b| b == b'\n')
            .count();
        u32::try_from(n + 1).unwrap_or(u32::MAX)
    }

    /// 1-based line of the member or item at `path`: its key's line for
    /// an object member, else its value's. 0 when it isn't there.
    #[must_use]
    pub fn line_of(&self, path: &[Segment]) -> u32 {
        let Some((last, parent)) = path.split_last() else { return 1 };
        let Some(parent) = self.at(parent) else { return 0 };
        let offset = match (last, &parent.kind) {
            (Segment::Key(k), NodeKind::Object(members)) => {
                members.iter().find(|m| &m.key == k).map(|m| m.key_span.start)
            }
            (Segment::Index(i), NodeKind::Array(items)) => items.get(*i).map(|n| n.span.start),
            _ => None,
        };
        offset.map_or(0, |o| self.line(o))
    }

    /// Sets the member at `path` to `value`, creating the objects on the
    /// way that are missing. Returns the new text, or `None` when a step
    /// of the path isn't an object.
    #[must_use]
    pub fn set(&self, path: &[Segment], value: &Value) -> Option<String> {
        let (obj, rest) = self.deepest(path)?;
        if !matches!(rest.first()?, Segment::Key(_)) {
            return None;
        }
        if rest.len() == 1
            && let Segment::Key(k) = &rest[0]
            && let Some(m) = obj.members().iter().find(|m| &m.key == k)
        {
            return Some(self.splice(m.value.span.clone(), &self.render(value, &m.value)));
        }
        let nested = nest(&rest[1..], value)?;
        let Segment::Key(key) = &rest[0] else { return None };
        self.insert_member(obj, key, &nested)
    }

    /// Appends `value` to the array at `path`, creating it (and the
    /// objects on the way) when it is missing.
    #[must_use]
    pub fn append(&self, path: &[Segment], value: &Value) -> Option<String> {
        match self.at(path) {
            Some(node @ Node { kind: NodeKind::Array(items), .. }) => {
                let text = compact(value);
                Some(match items.last() {
                    Some(last) => {
                        let sep = if self.same_line(node.span.start, last.span.start) {
                            ", ".to_string()
                        } else {
                            format!(",{}{}", self.newline(), self.indent_at(last.span.start))
                        };
                        self.splice(last.span.end..last.span.end, &format!("{sep}{text}"))
                    }
                    None if self.text[node.span.start + 1..node.span.end - 1].trim().is_empty() => {
                        self.splice(node.span.start + 1..node.span.end - 1, &text)
                    }
                    // Only comments inside: keep them after the new item.
                    None => self.splice(node.span.start + 1..node.span.start + 1, &text),
                })
            }
            Some(_) => None,
            None => self.set(path, &Value::Array(vec![value.clone()])),
        }
    }

    /// Removes the items of the array at `path` that equal `value`.
    /// Returns `None` when there are none.
    #[must_use]
    pub fn remove_item(&self, path: &[Segment], value: &Value) -> Option<String> {
        let items = self.at(path)?.items();
        let i = items.iter().position(|n| self.value(n).as_ref() == Some(value))?;
        let range = if let Some(next) = items.get(i + 1) {
            items[i].span.start..next.span.start
        } else if i > 0 {
            items[i - 1].span.end..items[i].span.end
        } else {
            items[i].span.clone()
        };
        let text = self.splice(range, "");
        // Remove every equal item, one at a time.
        Some(JsoncDoc::parse(text.clone()).and_then(|d| d.remove_item(path, value)).unwrap_or(text))
    }

    /// The deepest object along `path` that exists, and the rest of the
    /// path below it.
    fn deepest<'p>(&self, path: &'p [Segment]) -> Option<(&Node, &'p [Segment])> {
        let mut node = &self.root;
        let mut i = 0;
        while i + 1 < path.len() {
            let next = match &path[i] {
                Segment::Key(k) => node.get(k),
                Segment::Index(_) => return None,
            };
            match next {
                Some(n @ Node { kind: NodeKind::Object(_), .. }) => node = n,
                Some(_) => return None,
                None => break,
            }
            i += 1;
        }
        matches!(node.kind, NodeKind::Object(_)).then_some((node, &path[i..]))
    }

    /// Adds `"key": value_text` to `obj`.
    fn insert_member(&self, obj: &Node, key: &str, value: &Value) -> Option<String> {
        let key = serde_json::to_string(key).ok()?;
        let members = obj.members();
        if let Some(last) = members.last() {
            let indent = self.indent_at(last.key_span.start);
            let inline = self.same_line(obj.span.start, last.key_span.start);
            let rendered = self.pretty(value, &indent);
            let sep =
                if inline { ", ".to_string() } else { format!(",{}{indent}", self.newline()) };
            return Some(self.splice(
                last.value.span.end..last.value.span.end,
                &format!("{sep}{key}: {rendered}"),
            ));
        }
        let outer = self.indent_at(obj.span.start);
        let indent = format!("{outer}{}", self.indent_unit());
        let rendered = self.pretty(value, &indent);
        let inner = &self.text[obj.span.start + 1..obj.span.end - 1];
        if inner.trim().is_empty() {
            Some(self.splice(
                obj.span.start + 1..obj.span.end - 1,
                &format!("{nl}{indent}{key}: {rendered}{nl}{outer}", nl = self.newline()),
            ))
        } else {
            // Only comments inside: keep them after the new member.
            Some(self.splice(
                obj.span.start + 1..obj.span.start + 1,
                &format!("{}{indent}{key}: {rendered}", self.newline()),
            ))
        }
    }

    /// `value` rendered where `old` is: compact when `old` fits on its line.
    fn render(&self, value: &Value, old: &Node) -> String {
        let indent = self.indent_at(old.span.start);
        self.pretty(value, &indent)
    }

    /// `value` as JSON: scalars, short arrays and small objects of scalars
    /// on one line, anything bigger over several lines indented from
    /// `indent`.
    fn pretty(&self, value: &Value, indent: &str) -> String {
        let one_line = compact(value);
        if one_line.len() <= 60 || !matches!(value, Value::Object(_) | Value::Array(_)) {
            return one_line;
        }
        let unit = self.indent_unit();
        let inner = format!("{indent}{unit}");
        let nl = self.newline();
        let sep = format!(",{nl}");
        match value {
            Value::Object(map) => {
                let parts: Vec<String> = map
                    .iter()
                    .map(|(k, v)| {
                        format!(
                            "{inner}{}: {}",
                            serde_json::to_string(k).unwrap_or_default(),
                            self.pretty(v, &inner)
                        )
                    })
                    .collect();
                format!("{{{nl}{}{nl}{indent}}}", parts.join(&sep))
            }
            Value::Array(items) => {
                let parts: Vec<String> =
                    items.iter().map(|v| format!("{inner}{}", self.pretty(v, &inner))).collect();
                format!("[{nl}{}{nl}{indent}]", parts.join(&sep))
            }
            _ => one_line,
        }
    }

    /// The whitespace before the first non-blank character of the line
    /// holding `offset`.
    fn indent_at(&self, offset: usize) -> String {
        let start = self.text[..offset].rfind('\n').map_or(0, |i| i + 1);
        self.text[start..].chars().take_while(|c| *c == ' ' || *c == '\t').collect()
    }

    /// The document's indentation step, from its first indented member.
    fn indent_unit(&self) -> String {
        self.root
            .members()
            .first()
            .map(|m| self.indent_at(m.key_span.start))
            .filter(|i| !i.is_empty() && !self.same_line(self.root.span.start, 0))
            .unwrap_or_else(|| "  ".to_string())
    }

    /// The document's line ending: `\r\n` if it uses it anywhere.
    fn newline(&self) -> &'static str {
        if self.text.contains("\r\n") { "\r\n" } else { "\n" }
    }

    fn same_line(&self, a: usize, b: usize) -> bool {
        let (a, b) = (a.min(b), a.max(b));
        !self.text[a..b].contains('\n')
    }

    fn splice(&self, range: Range<usize>, with: &str) -> String {
        let mut out = String::with_capacity(self.text.len() + with.len());
        out.push_str(&self.text[..range.start]);
        out.push_str(with);
        out.push_str(&self.text[range.end..]);
        out
    }
}

/// `value` wrapped in an object for each key of `path`.
fn nest(path: &[Segment], value: &Value) -> Option<Value> {
    let mut v = value.clone();
    for seg in path.iter().rev() {
        let Segment::Key(k) = seg else { return None };
        let mut map = serde_json::Map::new();
        map.insert(k.clone(), v);
        v = Value::Object(map);
    }
    Some(v)
}

/// `value` on one line, with spaces after `:` and `,` as people write it.
fn compact(value: &Value) -> String {
    match value {
        Value::Object(map) if map.is_empty() => "{}".into(),
        Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| {
                    format!("{}: {}", serde_json::to_string(k).unwrap_or_default(), compact(v))
                })
                .collect();
            format!("{{ {} }}", parts.join(", "))
        }
        Value::Array(items) => {
            format!("[{}]", items.iter().map(compact).collect::<Vec<_>>().join(", "))
        }
        _ => value.to_string(),
    }
}

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
}

/// How deeply values may nest, so a hostile file can't overflow the stack.
const MAX_DEPTH: usize = 256;

impl Parser<'_> {
    /// Skips whitespace and comments.
    fn skip(&mut self) {
        loop {
            while self.pos < self.src.len() && self.src[self.pos].is_ascii_whitespace() {
                self.pos += 1;
            }
            match (self.src.get(self.pos), self.src.get(self.pos + 1)) {
                (Some(b'/'), Some(b'/')) => {
                    while self.pos < self.src.len() && self.src[self.pos] != b'\n' {
                        self.pos += 1;
                    }
                }
                (Some(b'/'), Some(b'*')) => {
                    self.pos += 2;
                    while self.pos < self.src.len()
                        && !(self.src[self.pos] == b'*'
                            && self.src.get(self.pos + 1) == Some(&b'/'))
                    {
                        self.pos += 1;
                    }
                    self.pos = (self.pos + 2).min(self.src.len());
                }
                _ => return,
            }
        }
    }

    fn value(&mut self, depth: usize) -> Option<Node> {
        if depth > MAX_DEPTH {
            return None;
        }
        let start = self.pos;
        match *self.src.get(self.pos)? {
            b'{' => {
                self.pos += 1;
                let mut members = Vec::new();
                loop {
                    self.skip();
                    match *self.src.get(self.pos)? {
                        b'}' => break,
                        b'"' => {}
                        _ => return None,
                    }
                    let key_start = self.pos;
                    self.string()?;
                    let key_span = key_start..self.pos;
                    let key: String = serde_json::from_slice(&self.src[key_span.clone()]).ok()?;
                    self.skip();
                    (self.src.get(self.pos)? == &b':').then_some(())?;
                    self.pos += 1;
                    self.skip();
                    let value = self.value(depth + 1)?;
                    members.push(Member { key, key_span, value });
                    self.skip();
                    match *self.src.get(self.pos)? {
                        b',' => self.pos += 1,
                        b'}' => break,
                        _ => return None,
                    }
                }
                self.pos += 1;
                Some(Node { span: start..self.pos, kind: NodeKind::Object(members) })
            }
            b'[' => {
                self.pos += 1;
                let mut items = Vec::new();
                loop {
                    self.skip();
                    if *self.src.get(self.pos)? == b']' {
                        break;
                    }
                    items.push(self.value(depth + 1)?);
                    self.skip();
                    match *self.src.get(self.pos)? {
                        b',' => self.pos += 1,
                        b']' => break,
                        _ => return None,
                    }
                }
                self.pos += 1;
                Some(Node { span: start..self.pos, kind: NodeKind::Array(items) })
            }
            b'"' => {
                self.string()?;
                Some(Node { span: start..self.pos, kind: NodeKind::Scalar })
            }
            _ => {
                while self.pos < self.src.len()
                    && !matches!(self.src[self.pos], b',' | b'}' | b']' | b'/')
                    && !self.src[self.pos].is_ascii_whitespace()
                {
                    self.pos += 1;
                }
                (self.pos > start).then_some(Node { span: start..self.pos, kind: NodeKind::Scalar })
            }
        }
    }

    /// Moves past a string starting at `pos`.
    fn string(&mut self) -> Option<()> {
        self.pos += 1;
        loop {
            match *self.src.get(self.pos)? {
                b'\\' => self.pos += 2,
                b'"' => {
                    self.pos += 1;
                    return Some(());
                }
                _ => self.pos += 1,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn path(keys: &[&str]) -> Vec<Segment> {
        keys.iter().map(|k| Segment::from(*k)).collect()
    }

    const TSCONFIG: &str = r#"{
  // base settings
  "extends": "./base.json",
  "compilerOptions": {
    "strict": true, // keep
  },
  "include": ["src"]
}
"#;

    #[test]
    fn spans_and_lines() {
        let doc = JsoncDoc::parse(TSCONFIG).unwrap();
        assert_eq!(doc.line_of(&path(&["compilerOptions", "strict"])), 5);
        assert_eq!(doc.line_of(&path(&["include"])), 7);
        assert_eq!(doc.line_of(&path(&["missing"])), 0);
        let include = doc.at(&path(&["include"])).unwrap();
        assert_eq!(doc.value(include), Some(json!(["src"])));
    }

    #[test]
    fn set_replaces_or_inserts_and_keeps_comments() {
        let doc = JsoncDoc::parse(TSCONFIG).unwrap();
        let out = doc.set(&path(&["compilerOptions", "skipLibCheck"]), &json!(true)).unwrap();
        assert!(out.contains("\"strict\": true,\n    \"skipLibCheck\": true, // keep"), "{out}");
        let out = doc.set(&path(&["compilerOptions", "strict"]), &json!(false)).unwrap();
        assert!(out.contains("\"strict\": false, // keep"), "{out}");
        let out = doc.set(&path(&["references"]), &json!([{ "path": "../a" }])).unwrap();
        assert!(
            out.contains("\"include\": [\"src\"],\n  \"references\": [{ \"path\": \"../a\" }]\n}"),
            "{out}"
        );
        assert!(JsoncDoc::parse(out).is_some());
    }

    #[test]
    fn set_creates_missing_objects() {
        let doc = JsoncDoc::parse("{\n  \"include\": [\"src\"]\n}").unwrap();
        let out = doc.set(&path(&["compilerOptions", "incremental"]), &json!(true)).unwrap();
        assert_eq!(
            out,
            "{\n  \"include\": [\"src\"],\n  \"compilerOptions\": { \"incremental\": true }\n}"
        );
        let doc = JsoncDoc::parse("{}").unwrap();
        let out = doc.set(&path(&["compilerOptions", "types"]), &json!([])).unwrap();
        assert_eq!(out, "{\n  \"compilerOptions\": { \"types\": [] }\n}");
    }

    #[test]
    fn append_and_remove_items() {
        let doc = JsoncDoc::parse(TSCONFIG).unwrap();
        let out = doc.append(&path(&["include"]), &json!("types")).unwrap();
        assert!(out.contains("\"include\": [\"src\", \"types\"]"), "{out}");
        let out = doc.append(&path(&["exclude"]), &json!("dist")).unwrap();
        assert!(out.contains("\"exclude\": [\"dist\"]"), "{out}");

        let doc = JsoncDoc::parse(
            "{\n  \"references\": [\n    { \"path\": \"../a\" },\n    { \"path\": \"../b\" }\n  ]\n}",
        )
        .unwrap();
        let out = doc.append(&path(&["references"]), &json!({ "path": "../c" })).unwrap();
        assert!(out.contains("{ \"path\": \"../b\" },\n    { \"path\": \"../c\" }\n  ]"), "{out}");
        let out = doc.remove_item(&path(&["references"]), &json!({ "path": "../b" })).unwrap();
        assert_eq!(out, "{\n  \"references\": [\n    { \"path\": \"../a\" }\n  ]\n}");
        let out = doc.remove_item(&path(&["references"]), &json!({ "path": "../a" })).unwrap();
        assert_eq!(out, "{\n  \"references\": [\n    { \"path\": \"../b\" }\n  ]\n}");
        assert!(doc.remove_item(&path(&["references"]), &json!({ "path": "../z" })).is_none());
    }

    #[test]
    fn keeps_crlf_and_comment_only_arrays() {
        let doc = JsoncDoc::parse("{\r\n  \"a\": 1\r\n}\r\n").unwrap();
        let out = doc.set(&path(&["b"]), &json!(2)).unwrap();
        assert_eq!(out, "{\r\n  \"a\": 1,\r\n  \"b\": 2\r\n}\r\n");
        let doc = JsoncDoc::parse("{ \"exclude\": [\n    // generated\n  ] }").unwrap();
        let out = doc.append(&path(&["exclude"]), &json!("dist")).unwrap();
        assert_eq!(out, "{ \"exclude\": [\"dist\"\n    // generated\n  ] }");
        assert!(JsoncDoc::parse(out).is_some());
    }

    #[test]
    fn rejects_broken_and_deep_input() {
        assert!(JsoncDoc::parse("{ \"a\": }").is_none());
        assert!(JsoncDoc::parse("{ \"a\": 1 } x").is_none());
        assert!(JsoncDoc::parse("[".repeat(10_000)).is_none());
    }
}
