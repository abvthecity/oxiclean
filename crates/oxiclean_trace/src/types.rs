//! `types.json`: one record per type the checker built, in order of id.
//!
//! The file grows with the number of types and can run to gigabytes, so
//! it is read only when a hot spot names a type, once, keeping only the
//! records of the types the hot spots name; the others are skipped
//! without being parsed. A file cut short, as when `tsc` ran out of
//! memory, keeps the records before the cut.

use std::{
    collections::{HashMap, HashSet},
    fmt,
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
};

use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, SeqAccess, Visitor};
use serde_json::{Map, Value};
use tracing::{debug, warn};

/// The records of a `types.json` that were asked for, by id.
#[derive(Debug, Default)]
pub(crate) struct TypesFile {
    records: HashMap<u64, Map<String, Value>>,
    /// How many records the file holds, read or not.
    count: u64,
}

impl TypesFile {
    /// Reads the records of `ids` from `path`; an unreadable or malformed
    /// file keeps what was read before the problem, with a warning.
    ///
    /// `tsc` writes one record per line, so lines are counted and only the
    /// ones asked for are parsed, up to the last of them. A file laid out
    /// otherwise, or whose records don't carry the ids of their lines, is
    /// read again as JSON.
    pub(crate) fn read(path: &Path, ids: &HashSet<u64>) -> Self {
        let Some(last) = ids.iter().copied().max() else { return Self::default() };
        let file = match File::open(path) {
            Ok(f) => f,
            Err(e) => {
                warn!("Can't read {}: {e}", path.display());
                return Self::default();
            }
        };
        match read_lines(BufReader::with_capacity(1 << 20, file), ids, last, path) {
            Ok(Some(out)) => return out,
            Ok(None) => debug!("{} isn't one type per line; reading it as JSON", path.display()),
            // The JSON reader keeps what comes before the error, and warns.
            Err(e) => {
                debug!("Reading {} line by line failed: {e}; reading it as JSON", path.display())
            }
        }
        let file = match File::open(path) {
            Ok(f) => f,
            Err(e) => {
                warn!("Can't read {}: {e}", path.display());
                return Self::default();
            }
        };
        let mut out = Self::default();
        let mut de = serde_json::Deserializer::from_reader(BufReader::with_capacity(1 << 16, file));
        if let Err(e) = (&mut de).deserialize_seq(Records { ids, out: &mut out }) {
            warn!(
                "{} is cut short or malformed ({e}); using its first {} types",
                path.display(),
                out.count
            );
        }
        out
    }

    /// Whether the file holds no types.
    pub(crate) fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The type with `id`, when it was asked for and the file has it.
    pub(crate) fn get(&self, id: u64) -> Option<&Map<String, Value>> {
        self.records.get(&id)
    }
}

/// Reads a types file written one record per line, as `tsc` writes it:
/// `[{"id":1,…},` then `{"id":2,…},` … `{"id":n,…}]`, with `null` for a
/// record left out. Only the lines of `ids` are parsed; the others are
/// found with `memchr` and checked by their shape and their `"id"`
/// prefix. `Ok(None)` when the file isn't laid out so, or a record's id
/// isn't its line's, so it's read as JSON instead.
fn read_lines(
    mut reader: impl BufRead,
    ids: &HashSet<u64>,
    last: u64,
    path: &Path,
) -> std::io::Result<Option<TypesFile>> {
    let mut out = TypesFile::default();
    let mut lines = Lines { started: false, done: false };
    // A line split across reads.
    let mut carry: Vec<u8> = Vec::new();
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            break;
        }
        let len = chunk.len();
        let mut from = 0;
        for nl in memchr::memchr_iter(b'\n', chunk) {
            let line = if carry.is_empty() {
                &chunk[from..nl]
            } else {
                carry.extend_from_slice(&chunk[from..nl]);
                &carry[..]
            };
            let ok = lines.add(line, ids, &mut out);
            carry.clear();
            from = nl + 1;
            if !ok {
                return Ok(None);
            }
            if lines.done || out.count >= last {
                return Ok(Some(out));
            }
        }
        carry.extend_from_slice(&chunk[from..]);
        reader.consume(len);
    }
    if !carry.is_empty() && !lines.add(&carry, ids, &mut out) {
        return Ok(None);
    }
    // No closing `]`: cut short, or not a list of types at all.
    if !lines.done && out.count == 0 {
        return Ok(None);
    }
    if !lines.done {
        warn!("{} is cut short; using its first {} types", path.display(), out.count);
    }
    Ok(Some(out))
}

/// Where [`read_lines`] is in the array.
struct Lines {
    /// Whether the `[` was read.
    started: bool,
    /// Whether the `]` was read.
    done: bool,
}

impl Lines {
    /// Reads one line into `out`; `false` when it isn't one record.
    fn add(&mut self, line: &[u8], ids: &HashSet<u64>, out: &mut TypesFile) -> bool {
        let mut line = line.trim_ascii();
        if !self.started {
            if line.is_empty() {
                return true;
            }
            // A byte order mark.
            line = line.strip_prefix(b"\xef\xbb\xbf").unwrap_or(line).trim_ascii_start();
            let Some(rest) = line.strip_prefix(b"[") else { return false };
            self.started = true;
            line = rest.trim_ascii_start();
        }
        if self.done {
            return line.is_empty();
        }
        // Records end in `}` or `l`, so a `]` at the end closes the array.
        if let Some(rest) = line.strip_suffix(b"]") {
            self.done = true;
            line = rest.trim_ascii_end();
        }
        let record = line.strip_suffix(b",").unwrap_or(line).trim_ascii_end();
        if record.is_empty() {
            return true;
        }
        let id = out.count + 1;
        if record == b"null" {
            out.count = id;
            return true;
        }
        if !(record.starts_with(b"{") && record.ends_with(b"}")) {
            return false;
        }
        // A second `{"id":` means more than one record on the line (a string
        // can't hold the unescaped quote), which only a full parse can read.
        if memchr::memmem::find(&record[1..], br#"{"id":"#).is_some() {
            return false;
        }
        // tsc writes the id first; when it's there it must be this line's.
        if let Some(rest) = record.strip_prefix(br#"{"id":"#) {
            let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
            let written = std::str::from_utf8(&rest[..digits]).ok().and_then(|d| d.parse().ok());
            if written != Some(id) {
                return false;
            }
        }
        if ids.contains(&id) {
            match serde_json::from_slice::<Map<String, Value>>(record) {
                Ok(map) if map.get("id").is_none_or(|v| v.as_u64() == Some(id)) => {
                    out.records.insert(id, map);
                }
                _ => return false,
            }
        }
        out.count = id;
        true
    }
}

/// Reads a JSON array's elements, the ones asked for into a map and the
/// others into nothing, keeping those before an error.
struct Records<'a> {
    ids: &'a HashSet<u64>,
    out: &'a mut TypesFile,
}

impl<'de> Visitor<'de> for Records<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an array of types")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        loop {
            // Ids start at 1, at index 0.
            let id = self.out.count + 1;
            let read = if self.ids.contains(&id) {
                seq.next_element::<Value>()?.map(|record| {
                    if let Value::Object(record) = record {
                        self.out.records.insert(id, record);
                    }
                })
            } else {
                seq.next_element::<IgnoredAny>()?.map(|_| ())
            };
            if read.is_none() {
                return Ok(());
            }
            self.out.count += 1;
        }
    }
}

impl<'de> DeserializeSeed<'de> for Records<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_seq(self)
    }
}

/// Where a type is declared or written, as `types.json` gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypeLocation {
    /// The file, as the trace writes it.
    pub path: String,
    /// 1-based line and column.
    pub line: u64,
    pub char: u64,
}

/// A type as analyze-trace's `simplifyType` describes it: its kind, the
/// symbol it is named after, and where it comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SimpleType {
    pub id: u64,
    /// e.g. `AliasedUnion`, `GenericInstantiation`, `Object`.
    pub kind: String,
    pub name: Option<String>,
    /// Members of a union or intersection.
    pub count: Option<usize>,
    pub location: Option<TypeLocation>,
    /// How `tsc` prints the type, kept where analyze-trace keeps it.
    pub display: Option<String>,
}

fn str_of<'a>(t: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    t.get(key).and_then(Value::as_str)
}

/// The flags of `t`; traces from TypeScript 4.1 hold one number instead
/// of names.
fn flags(t: &Map<String, Value>) -> Vec<String> {
    let names: Vec<String> = t
        .get("flags")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|f| f.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    match names.as_slice() {
        [one] if !one.is_empty() && one.bytes().all(|b| b.is_ascii_digit()) => {
            let bits: u64 = one.parse().unwrap_or(0);
            const NAMES: [&str; 29] = [
                "Any",
                "Unknown",
                "String",
                "Number",
                "Boolean",
                "Enum",
                "BigInt",
                "StringLiteral",
                "NumberLiteral",
                "BooleanLiteral",
                "EnumLiteral",
                "BigIntLiteral",
                "ESSymbol",
                "UniqueESSymbol",
                "Void",
                "Undefined",
                "Null",
                "Never",
                "TypeParameter",
                "Object",
                "Union",
                "Intersection",
                "Index",
                "IndexedAccess",
                "Conditional",
                "Substitution",
                "NonPrimitive",
                "TemplateLiteral",
                "StringMapping",
            ];
            NAMES
                .iter()
                .enumerate()
                .filter(|(i, _)| bits & (1 << i) != 0)
                .map(|(_, n)| (*n).to_string())
                .collect()
        }
        _ => names,
    }
}

/// Where `t` comes from: a destructuring pattern, the reference that made
/// it, or its symbol's first declaration, in that order.
fn location(t: &Map<String, Value>) -> Option<TypeLocation> {
    let node = ["destructuringPattern", "referenceLocation", "firstDeclaration"]
        .iter()
        .find_map(|k| t.get(*k).filter(|v| !v.is_null()))?;
    let start = node.get("start");
    Some(TypeLocation {
        path: node.get("path")?.as_str()?.to_string(),
        line: start.and_then(|s| s.get("line")).and_then(Value::as_u64)?,
        char: start.and_then(|s| s.get("character")).and_then(Value::as_u64)?,
    })
}

/// `t` described as analyze-trace's `simplifyType` describes it.
pub(crate) fn simplify(id: u64, t: &Map<String, Value>) -> SimpleType {
    let name = str_of(t, "symbolName").map(str::to_string);
    let location = location(t);
    let display = str_of(t, "display").map(str::to_string);
    let flags = flags(t);
    let has = |key: &str| t.get(key).is_some_and(|v| !v.is_null() && *v != Value::Bool(false));
    let aliased =
        |kind: &str| if name.is_some() { format!("Aliased{kind}") } else { kind.to_string() };
    let array_len = |key: &str| t.get(key).and_then(Value::as_array).map(Vec::len);
    let simple = |kind: String| SimpleType {
        id,
        kind,
        name: name.clone(),
        count: None,
        location: location.clone(),
        display: None,
    };

    if let Some(intrinsic) = str_of(t, "intrinsicName") {
        return SimpleType { name: Some(intrinsic.to_string()), ..simple("Intrinsic".into()) };
    }
    if has("unionTypes") {
        return SimpleType { count: array_len("unionTypes"), ..simple(aliased("Union")) };
    }
    if has("intersectionTypes") {
        return SimpleType {
            count: array_len("intersectionTypes"),
            ..simple(aliased("Intersection"))
        };
    }
    for (key, kind) in [
        ("indexedAccessObjectType", "IndexedAccess"),
        ("keyofType", "IndexType"),
        ("isTuple", "Tuple"),
        ("conditionalCheckType", "ConditionalType"),
        ("substitutionBaseType", "SubstitutionType"),
        ("reverseMappedSourceType", "ReverseMappedType"),
    ] {
        if has(key) {
            return simple(aliased(kind));
        }
    }
    if has("aliasTypeArguments") {
        return simple("GenericTypeAlias".into());
    }
    let instantiated = t.get("instantiatedType").and_then(Value::as_u64);
    if instantiated.is_some() && array_len("typeArguments").unwrap_or(0) > 0 {
        let kind = if instantiated == Some(id) { "GenericType" } else { "GenericInstantiation" };
        return simple(kind.into());
    }
    if has("destructuringPattern") {
        return simple("Destructuring".into());
    }
    for (flag, kind) in [
        ("StringLiteral", "StringLiteral"),
        ("NumberLiteral", "NumberLiteral"),
        ("BigIntLiteral", "BigIntLiteral"),
    ] {
        if flags.iter().any(|f| f == flag) {
            return SimpleType { display, ..simple(kind.into()) };
        }
    }
    if flags.iter().any(|f| f == "TypeParameter") {
        return simple("TypeParameter".into());
    }
    if flags.iter().any(|f| f == "UniqueESSymbol") {
        return simple("Unique".into());
    }
    if let Some(n) = name.as_deref().and_then(|n| n.strip_prefix("__@")) {
        let known = n
            .rsplit_once('@')
            .filter(|(_, d)| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()));
        let symbol = known.map_or_else(|| format!("__@{n}"), |(s, _)| s.to_string());
        return SimpleType { name: Some(symbol), ..simple("KnownSymbol".into()) };
    }
    let anonymous = |kind: String| SimpleType {
        name: None,
        display: if location.is_some() { None } else { display.clone() },
        ..simple(kind)
    };
    match name.as_deref() {
        Some(n @ ("__function" | "__type" | "__class" | "__object")) => {
            let rest = &n[2..];
            let mut kind = String::from("Anonymous");
            kind.push_str(&rest[..1].to_uppercase());
            kind.push_str(&rest[1..]);
            return anonymous(kind);
        }
        Some("__jsxAttributes") => return anonymous("JsxAttributesType".into()),
        _ => {}
    }
    if flags.iter().any(|f| f == "Object") && name.is_some() {
        return simple("Object".into());
    }
    if display.as_deref().is_some_and(|d| d.starts_with("(props:") && d.ends_with("=> Element")) {
        return SimpleType { display, ..simple("JsxElementSignature".into()) };
    }
    SimpleType { display, ..simple("Other".into()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn parse(json: &str) -> Map<String, Value> {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn simplifies_as_analyze_trace() {
        let t = parse(
            r#"{"id":5,"symbolName":"Cls","unionTypes":[1,2,3],"flags":["Union"],"firstDeclaration":{"path":"/r/a.ts","start":{"line":3,"character":1},"end":{"line":3,"character":40}}}"#,
        );
        let s = simplify(5, &t);
        assert_eq!(
            (s.kind.as_str(), s.name.as_deref(), s.count),
            ("AliasedUnion", Some("Cls"), Some(3))
        );
        assert_eq!(s.location, Some(TypeLocation { path: "/r/a.ts".into(), line: 3, char: 1 }));

        let lit = simplify(7, &parse(r#"{"id":7,"flags":["StringLiteral"],"display":"\"a\""}"#));
        assert_eq!((lit.kind.as_str(), lit.display.as_deref()), ("StringLiteral", Some("\"a\"")));
        let anon = simplify(
            8,
            &parse(r#"{"id":8,"symbolName":"__type","flags":["Object"],"display":"{ a: 1; }"}"#),
        );
        assert_eq!(
            (anon.kind.as_str(), anon.name, anon.display.as_deref()),
            ("AnonymousType", None, Some("{ a: 1; }"))
        );
        let inst = simplify(
            9,
            &parse(
                r#"{"id":9,"symbolName":"Array","instantiatedType":4,"typeArguments":[14],"flags":["Object"]}"#,
            ),
        );
        assert_eq!(inst.kind, "GenericInstantiation");
        let sym =
            simplify(10, &parse(r#"{"id":10,"symbolName":"__@iterator@46","flags":["Object"]}"#));
        assert_eq!((sym.kind.as_str(), sym.name.as_deref()), ("KnownSymbol", Some("iterator")));
        let old = simplify(11, &parse(r#"{"id":11,"flags":["262144"]}"#));
        assert_eq!(old.kind, "TypeParameter");
    }

    #[test]
    fn a_file_cut_short_keeps_what_came_before() {
        let t = TempDir::new().unwrap();
        let path = t.path().join("types.json");
        fs::write(&path, "[{\"id\":1,\"intrinsicName\":\"any\"},\n{\"id\":2,\"intrinsicName\":\"string\"},\nnull,\n{\"id\":4,\"intr").unwrap();
        let ids: HashSet<u64> = [0, 2, 3, 4].into();
        let types = TypesFile::read(&path, &ids);
        assert_eq!(
            types.get(2).and_then(|t| t.get("intrinsicName").cloned()),
            Some(Value::from("string"))
        );
        assert!(types.get(1).is_none(), "type 1 wasn't asked for");
        assert!(types.get(3).is_none() && types.get(4).is_none() && types.get(0).is_none());
        assert!(!types.is_empty());
        assert!(TypesFile::read(&t.path().join("missing.json"), &ids).is_empty());
        fs::write(&path, "{\"not\": \"an array\"}").unwrap();
        assert!(TypesFile::read(&path, &ids).is_empty());
        // A record that isn't an object loses only itself.
        fs::write(&path, "[1, {\"id\":2}]").unwrap();
        let types = TypesFile::read(&path, &[1, 2].into());
        assert!(types.get(1).is_none() && types.get(2).is_some());
    }
    #[test]
    fn reads_records_however_they_are_laid_out() {
        let t = TempDir::new().unwrap();
        let path = t.path().join("types.json");
        let ids: HashSet<u64> = [2, 3, 9].into();
        for json in [
            // tsc, one record a line.
            "[{\"id\":1,\"flags\":[]},\n{\"id\":2,\"symbolName\":\"B\"},\n{\"id\":3,\"symbolName\":\"C\"}]\n",
            // Several records a line.
            "[{\"id\":1,\"flags\":[]},{\"id\":2,\"symbolName\":\"B\"},\n{\"id\":3,\"symbolName\":\"C\"}]",
            // All on one line, and pretty-printed.
            "[{\"id\":1},{\"id\":2,\"symbolName\":\"B\"},{\"id\":3,\"symbolName\":\"C\"}]",
            "[\n  {\n    \"id\": 1\n  },\n  {\"id\": 2, \"symbolName\": \"B\"},\n  {\"id\":3,\"symbolName\":\"C\"}\n]",
        ] {
            fs::write(&path, json).unwrap();
            let types = TypesFile::read(&path, &ids);
            let name = |id| types.get(id).and_then(|t| t.get("symbolName").cloned());
            assert_eq!((name(2), name(3)), (Some("B".into()), Some("C".into())), "{json}");
            assert!(types.get(1).is_none() && types.get(9).is_none(), "{json}");
        }
    }
}
