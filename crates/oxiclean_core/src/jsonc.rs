//! Minimal JSONC support for tsconfig.json files.
//!
//! tsconfig files allow `//` and `/* */` comments and trailing commas, none of
//! which `serde_json` accepts. This strips them while leaving string contents
//! (e.g. `"$schema": "https://..."`) untouched.

/// Parse a JSON-with-comments document, tolerating trailing commas.
pub fn parse_jsonc(src: &str) -> Option<serde_json::Value> {
    serde_json::from_str(&strip_jsonc(src)).ok()
}

fn strip_jsonc(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let mut in_string = false;

    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            out.push(c);
            if c == b'\\' && i + 1 < bytes.len() {
                out.push(bytes[i + 1]);
                i += 2;
                continue;
            }
            if c == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }

        match c {
            b'"' => {
                in_string = true;
                out.push(c);
                i += 1;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i += 2;
            }
            b',' => {
                // Drop the comma if the next significant character closes a container
                let mut j = i + 1;
                loop {
                    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if bytes.get(j) == Some(&b'/') && bytes.get(j + 1) == Some(&b'/') {
                        while j < bytes.len() && bytes[j] != b'\n' {
                            j += 1;
                        }
                    } else if bytes.get(j) == Some(&b'/') && bytes.get(j + 1) == Some(&b'*') {
                        j += 2;
                        while j < bytes.len()
                            && !(bytes[j] == b'*' && bytes.get(j + 1) == Some(&b'/'))
                        {
                            j += 1;
                        }
                        j += 2;
                    } else {
                        break;
                    }
                }
                if !matches!(bytes.get(j), Some(b'}') | Some(b']')) {
                    out.push(c);
                }
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }

    // Only ASCII bytes were removed, so the output is still valid UTF-8
    String::from_utf8(out).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keeps_slashes_inside_strings() {
        let v = parse_jsonc(r#"{ "$schema": "https://json.schemastore.org/tsconfig" }"#).unwrap();
        assert_eq!(v["$schema"], "https://json.schemastore.org/tsconfig");
    }

    #[test]
    fn test_strips_line_and_block_comments() {
        let v = parse_jsonc(
            r#"{
              // line
              /* block
                 comment */
              "a": 1 /* inline */
            }"#,
        )
        .unwrap();
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn test_trailing_commas() {
        let v = parse_jsonc(
            r#"{ "a": [1, 2,], "b": { "c": 3, // note
        }, }"#,
        )
        .unwrap();
        assert_eq!(v["a"].as_array().unwrap().len(), 2);
        assert_eq!(v["b"]["c"], 3);
    }

    #[test]
    fn test_escaped_quotes_in_strings() {
        let v = parse_jsonc(r#"{ "a": "say \"hi\" // not a comment" }"#).unwrap();
        assert_eq!(v["a"], "say \"hi\" // not a comment");
    }
}
