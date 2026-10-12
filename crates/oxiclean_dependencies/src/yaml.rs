//! A reader for the subset of YAML that pnpm's files use.

/// A `key: value` line of a YAML block mapping, with the keys of the
/// mappings it is nested in.
#[derive(Debug, PartialEq)]
pub(crate) struct MappingLine {
    pub(crate) path: Vec<String>,
    /// The scalar value, or `None` for a key that opens a nested mapping.
    pub(crate) value: Option<String>,
    /// 1-based line.
    pub(crate) line: u32,
}

/// The mapping lines of `yaml`, read as far as pnpm-workspace.yaml needs:
/// block mappings of scalars, nested by indentation, with comments and
/// quotes. Sequence items and flow collections are skipped.
pub(crate) fn mapping_lines(yaml: &str) -> Vec<MappingLine> {
    let mut out = Vec::new();
    let mut parents: Vec<(usize, String)> = Vec::new();
    for (i, raw) in yaml.lines().enumerate() {
        let text = raw.trim_start();
        let indent = raw.len() - text.len();
        if text.is_empty() || text.starts_with('#') || text.starts_with("---") {
            continue;
        }
        while parents.last().is_some_and(|(at, _)| *at >= indent) {
            parents.pop();
        }
        if text.starts_with('-') {
            continue;
        }
        let Some((key, rest)) = split_key(text) else { continue };
        let value = scalar(rest);
        let mut path: Vec<String> = parents.iter().map(|(_, k)| k.clone()).collect();
        path.push(key.clone());
        if value.is_none() {
            parents.push((indent, key));
        }
        out.push(MappingLine { path, value, line: u32::try_from(i + 1).unwrap_or(u32::MAX) });
    }
    out
}

/// `key` and the rest of a `key: value` line, the key unquoted.
fn split_key(text: &str) -> Option<(String, &str)> {
    if let Some(q) = text.chars().next().filter(|c| matches!(c, '\'' | '"')) {
        let close = text[1..].find(q)? + 1;
        let rest = text[close + 1..].trim_start().strip_prefix(':')?;
        return Some((text[1..close].to_string(), rest));
    }
    let colon = text
        .match_indices(':')
        .find(|(i, _)| text[i + 1..].chars().next().is_none_or(char::is_whitespace))?;
    Some((text[..colon.0].trim_end().to_string(), &text[colon.0 + 1..]))
}

/// The scalar after a key, unquoted and without a trailing comment, or
/// `None` when the key opens a nested block.
fn scalar(rest: &str) -> Option<String> {
    let rest = rest.trim();
    if rest.is_empty() || rest.starts_with('#') {
        return None;
    }
    if let Some(q) = rest.chars().next().filter(|c| matches!(c, '\'' | '"')) {
        let close = rest[1..].find(q).map_or(rest.len(), |c| c + 1);
        return Some(rest[1..close].to_string());
    }
    Some(rest.split(" #").next().unwrap_or(rest).trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_nested_mappings() {
        let yaml = r#"packages:
  - packages/*

catalog:
  react: ^19.0.0 # pinned
  '@types/node': "22"

catalogs:
  # legacy apps
  react18:
    react: ^18.0.0
overrides:
  "foo@1>bar": catalog:
"#;
        let got: Vec<(String, Option<String>, u32)> =
            mapping_lines(yaml).into_iter().map(|l| (l.path.join("."), l.value, l.line)).collect();
        let s = |v: &str| Some(v.to_string());
        assert_eq!(
            got,
            [
                ("packages".into(), None, 1),
                ("catalog".into(), None, 4),
                ("catalog.react".into(), s("^19.0.0"), 5),
                ("catalog.@types/node".into(), s("22"), 6),
                ("catalogs".into(), None, 8),
                ("catalogs.react18".into(), None, 10),
                ("catalogs.react18.react".into(), s("^18.0.0"), 11),
                ("overrides".into(), None, 12),
                ("overrides.foo@1>bar".into(), s("catalog:"), 13),
            ]
        );
    }
}
