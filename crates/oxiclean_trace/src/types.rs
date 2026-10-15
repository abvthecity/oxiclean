//! `types.json`: one record per type the checker built, in order of id.
//!
//! The file grows with the number of types and can run to gigabytes, so
//! it is read only when a hot spot names a type, each record is kept as
//! raw text, and only the records a report needs are parsed. A file cut
//! short, as when `tsc` ran out of memory, keeps the records before the
//! cut.

use std::{fmt, fs::File, io::BufReader, path::Path};

use serde::de::{DeserializeSeed, Deserializer, SeqAccess, Visitor};
use serde_json::{Map, Value, value::RawValue};
use tracing::warn;

/// The records of a `types.json`, unparsed.
#[derive(Debug, Default)]
pub(crate) struct TypesFile {
    records: Vec<Box<RawValue>>,
}

impl TypesFile {
    /// Reads `path`; an unreadable or malformed file keeps what was read
    /// before the problem, with a warning.
    pub(crate) fn read(path: &Path) -> Self {
        let file = match File::open(path) {
            Ok(f) => f,
            Err(e) => {
                warn!("Can't read {}: {e}", path.display());
                return Self::default();
            }
        };
        let mut records = Vec::new();
        let mut de = serde_json::Deserializer::from_reader(BufReader::new(file));
        if let Err(e) = (&mut de).deserialize_seq(Records(&mut records)) {
            warn!(
                "{} is cut short or malformed ({e}); using its first {} types",
                path.display(),
                records.len()
            );
        }
        Self { records }
    }

    /// Whether the file holds no types.
    pub(crate) fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// The type with `id`, as its record says.
    pub(crate) fn get(&self, id: u64) -> Option<Map<String, Value>> {
        let index = usize::try_from(id.checked_sub(1)?).ok()?;
        serde_json::from_str(self.records.get(index)?.get()).ok()
    }
}

/// Collects a JSON array's elements into a vector, keeping those before an
/// error.
struct Records<'v>(&'v mut Vec<Box<RawValue>>);

impl<'de> DeserializeSeed<'de> for Records<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for Records<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an array of types")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while let Some(record) = seq.next_element::<Box<RawValue>>()? {
            self.0.push(record);
        }
        Ok(())
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
        fs::write(&path, "[{\"id\":1,\"intrinsicName\":\"any\"},\n{\"id\":2,\"intrinsicName\":\"string\"},\n{\"id\":3,\"intr").unwrap();
        let types = TypesFile::read(&path);
        assert_eq!(
            types.get(2).and_then(|t| t.get("intrinsicName").cloned()),
            Some(Value::from("string"))
        );
        assert!(types.get(3).is_none() && types.get(0).is_none());
        assert!(TypesFile::read(&t.path().join("missing.json")).get(1).is_none());
        fs::write(&path, "{\"not\": \"an array\"}").unwrap();
        assert!(TypesFile::read(&path).get(1).is_none());
    }
}
