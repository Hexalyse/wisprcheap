//! Adding a term to the `dictionary:` list of the YAML config, editing the text in place so comments and
//! formatting stay byte-for-byte the same.

use std::path::Path;

use anyhow::{Result, anyhow, bail};
use serde_yaml::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddTermResult {
    Added,
    Exists,
}

/// Scribe keyterm limits, so every added term is usable for transcription too.
pub fn validate_term(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("nothing selected");
    }
    if trimmed.contains(['\r', '\n']) {
        bail!("the selection spans several lines");
    }
    let term = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    if term.encode_utf16().count() >= 50 {
        let head: String = term.chars().take(30).collect();
        bail!("\"{head}...\" is too long (50 characters max)");
    }
    if term.split(' ').count() > 5 {
        bail!("\"{term}\" has more than 5 words");
    }
    if term.contains(['<', '>', '{', '}', '[', ']', '\\']) {
        bail!("\"{term}\" contains one of < > {{ }} [ ] \\");
    }
    Ok(term)
}

fn dictionary_terms(doc: &Value) -> Vec<String> {
    let Some(seq) = doc.get("dictionary").and_then(Value::as_sequence) else {
        return Vec::new();
    };
    seq.iter()
        .filter_map(|item| match item {
            Value::String(s) => Some(s.clone()),
            Value::Mapping(_) => item.get("term").and_then(Value::as_str).map(String::from),
            _ => None,
        })
        .collect()
}

fn without_dictionary(doc: &Value) -> Value {
    let mut doc = doc.clone();
    if let Some(map) = doc.as_mapping_mut() {
        map.remove("dictionary");
    }
    doc
}

/// The term as a YAML scalar: plain when that reads back identically, else double-quoted.
fn yaml_scalar(term: &str, flow: bool) -> String {
    let probe = if flow {
        format!("[{term}]")
    } else {
        format!("- {term}")
    };
    let plain_ok = matches!(
        serde_yaml::from_str::<Value>(&probe),
        Ok(Value::Sequence(seq)) if seq.len() == 1 && seq[0].as_str() == Some(term)
    );
    if plain_ok {
        term.to_string()
    } else {
        serde_json::to_string(term).unwrap_or_else(|_| format!("\"{term}\""))
    }
}

/// Byte ranges of the lines of `source`: (start, end without the line break, end including it).
fn lines(source: &str) -> Vec<(usize, usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    let bytes = source.as_bytes();
    while start < source.len() {
        let next = source[start..]
            .find('\n')
            .map(|i| start + i + 1)
            .unwrap_or(source.len());
        let mut end = if next > start && bytes[next - 1] == b'\n' {
            next - 1
        } else {
            next
        };
        if end > start && bytes[end - 1] == b'\r' {
            end -= 1;
        }
        out.push((start, end, next));
        start = next;
    }
    out
}

/// Index of the closing `]` of the flow sequence starting at `open` (quote-aware).
fn matching_bracket(source: &str, open: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'[' | b'{' => depth += 1,
            b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return (bytes[i] == b']').then_some(i);
                }
            }
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'\'' => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\'' {
                        if bytes.get(i + 1) == Some(&b'\'') {
                            i += 1;
                        } else {
                            break;
                        }
                    }
                    i += 1;
                }
            }
            b'#' if i > 0 && matches!(bytes[i - 1], b' ' | b'\t' | b'\n') => {
                // comment inside a multi-line flow sequence: skip to the end of the line
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Strip a trailing ` # comment` from the part of a line after the key (no quotes expected there).
fn strip_comment(s: &str) -> &str {
    let mut prev_space = true;
    for (i, c) in s.char_indices() {
        if c == '#' && prev_space {
            return &s[..i];
        }
        prev_space = c == ' ' || c == '\t';
    }
    s
}

fn is_dictionary_key(line: &str) -> Option<usize> {
    let rest = line.strip_prefix("dictionary")?;
    let after_spaces = rest.trim_start_matches([' ', '\t']);
    let colon = after_spaces.strip_prefix(':')?;
    if !colon.is_empty() && !colon.starts_with([' ', '\t']) {
        return None;
    }
    Some(line.len() - colon.len())
}

/// Insert one list item as text. None when the layout isn't one we can edit safely.
fn insert_item(source: &str, term: &str) -> Option<String> {
    let newline = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let all = lines(source);
    let Some(key_index) = all
        .iter()
        .position(|&(s, e, _)| is_dictionary_key(&source[s..e]).is_some())
    else {
        // Missing: append a new block list at the end.
        let mut out = source.to_string();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(newline);
        }
        out.push_str(&format!(
            "dictionary:{newline}  - {}{newline}",
            yaml_scalar(term, false)
        ));
        return Some(out);
    };
    let (ls, le, _) = all[key_index];
    let value_start = ls + is_dictionary_key(&source[ls..le])?;
    let value = strip_comment(&source[value_start..le]).trim();

    if value.starts_with('[') {
        let open = value_start + source[value_start..].find('[')?;
        let close = matching_bracket(source, open)?;
        let inner = source[open + 1..close].trim();
        let scalar = yaml_scalar(term, true);
        let insert = if inner.is_empty() {
            scalar
        } else {
            format!(", {scalar}")
        };
        return Some(format!(
            "{}{}{}",
            &source[..close],
            insert,
            &source[close..]
        ));
    }

    let scalar = yaml_scalar(term, false);
    if value == "null" || value == "~" {
        // `dictionary: null` -> `dictionary:` + a new item
        let value_pos = value_start + source[value_start..le].find(value)?;
        let head = source[..value_pos].trim_end_matches([' ', '\t']);
        return Some(format!(
            "{head}{newline}  - {scalar}{}",
            &source[value_pos + value.len()..]
        ));
    }
    if !value.is_empty() {
        return None;
    }

    // Block list (or empty value): find the item lines that follow the key.
    let mut first_indent: Option<String> = None;
    let mut last_content_end: Option<usize> = None;
    for &(s, e, _) in &all[key_index + 1..] {
        let line = &source[s..e];
        let trimmed = line.trim_start_matches([' ', '\t']);
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indented = trimmed.len() != line.len();
        let dash_item = trimmed == "-" || trimmed.starts_with("- ") || trimmed.starts_with("-\t");
        match &first_indent {
            None => {
                if !dash_item {
                    if indented {
                        return None; // a mapping or scalar we don't know how to extend
                    }
                    break;
                }
                first_indent = Some(line[..line.len() - trimmed.len()].to_string());
            }
            Some(indent) => {
                let continues = if indent.is_empty() {
                    indented || dash_item
                } else {
                    indented
                };
                if !continues {
                    break;
                }
            }
        }
        last_content_end = Some(e);
    }
    match (first_indent, last_content_end) {
        (Some(indent), Some(end)) => Some(format!(
            "{}{newline}{indent}- {scalar}{}",
            &source[..end],
            &source[end..]
        )),
        _ => Some(format!(
            "{}{newline}  - {scalar}{}",
            &source[..le],
            &source[le..]
        )),
    }
}

/// Append `term` to the `dictionary:` list of a YAML config file.
pub fn add_dictionary_term(file: &Path, raw_term: &str) -> Result<(String, AddTermResult)> {
    let _edit = crate::config::EDIT_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let term = validate_term(raw_term)?;
    let source =
        std::fs::read_to_string(file).map_err(|e| anyhow!("can't read {}: {e}", file.display()))?;
    let parsed: Value = if source.trim().is_empty() {
        Value::Null
    } else {
        serde_yaml::from_str(&source).map_err(|e| anyhow!("can't edit {}: {e}", file.display()))?
    };
    let existing = dictionary_terms(&parsed);
    if existing
        .iter()
        .any(|t| t.to_lowercase() == term.to_lowercase())
    {
        return Ok((term, AddTermResult::Exists));
    }

    let unsafe_err = || anyhow!("could not update {} safely", file.display());
    let updated = insert_item(&source, &term).ok_or_else(unsafe_err)?;

    // Never write a file we can't read back correctly: same document, one more term.
    let reparsed: Value = serde_yaml::from_str(&updated).map_err(|_| unsafe_err())?;
    let mut expected = existing;
    expected.push(term.clone());
    let original_rest = if parsed.is_null() {
        Value::Mapping(Default::default())
    } else {
        without_dictionary(&parsed)
    };
    if dictionary_terms(&reparsed) != expected || without_dictionary(&reparsed) != original_rest {
        return Err(unsafe_err());
    }
    std::fs::write(file, updated)?;
    Ok((term, AddTermResult::Added))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGINAL: &str = "# my config
dictionary:
  - git   # vcs
  - term: WisprFlow
    soundsLike: [Whisper Flow]

output:
  paste: true              # aligned comment
";

    fn temp_file(text: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let file = std::env::temp_dir().join(format!(
            "wisprcheap-dict-{}-{nanos}.yaml",
            std::process::id()
        ));
        std::fs::write(&file, text).unwrap();
        file
    }

    fn add(text: &str, term: &str) -> (String, AddTermResult) {
        let file = temp_file(text);
        let (_, result) = add_dictionary_term(&file, term).unwrap();
        let out = std::fs::read_to_string(&file).unwrap();
        let _ = std::fs::remove_file(&file);
        (out, result)
    }

    #[test]
    fn adds_one_line_and_keeps_everything_else() {
        let (out, result) = add(ORIGINAL, "  Kubernetes ");
        assert_eq!(result, AddTermResult::Added);
        assert_eq!(
            out,
            ORIGINAL.replace(
                "    soundsLike: [Whisper Flow]\n",
                "    soundsLike: [Whisper Flow]\n  - Kubernetes\n"
            )
        );
    }

    #[test]
    fn duplicates_are_detected_case_insensitively() {
        assert_eq!(
            add(ORIGINAL, "GIT"),
            (ORIGINAL.to_string(), AddTermResult::Exists)
        );
        assert_eq!(
            add(ORIGINAL, "wisprflow"),
            (ORIGINAL.to_string(), AddTermResult::Exists)
        );
    }

    #[test]
    fn quotes_terms_that_need_it() {
        let (out, _) = add(ORIGINAL, "Node: runtime");
        assert!(out.contains("- \"Node: runtime\"\n"), "{out}");
    }

    #[test]
    fn inline_crlf_and_missing() {
        assert_eq!(
            add("dictionary: [git, Codex]  # inline\n", "pnpm").0,
            "dictionary: [git, Codex, pnpm]  # inline\n"
        );
        assert_eq!(
            add(
                "dictionary:\r\n  - git\r\n\r\nsounds:\r\n  volume: 0.2\r\n",
                "pnpm"
            )
            .0,
            "dictionary:\r\n  - git\r\n  - pnpm\r\n\r\nsounds:\r\n  volume: 0.2\r\n"
        );
        assert_eq!(
            add("# comment\npolish:\n  enabled: true\n", "Bun").0,
            "# comment\npolish:\n  enabled: true\ndictionary:\n  - Bun\n"
        );
        assert_eq!(add("dictionary: []\n", "Bun").0, "dictionary: [Bun]\n");
        assert_eq!(
            add("dictionary:\nsounds: {}\n", "Bun").0,
            "dictionary:\n  - Bun\nsounds: {}\n"
        );
        assert_eq!(
            add("dictionary: null # x\n", "Bun").0,
            "dictionary:\n  - Bun # x\n"
        );
        assert_eq!(
            add("dictionary:\n- a\n- b\nx: 1\n", "Bun").0,
            "dictionary:\n- a\n- b\n- Bun\nx: 1\n"
        );
    }

    #[test]
    fn rejects_what_scribe_would_reject() {
        assert!(
            validate_term("")
                .unwrap_err()
                .to_string()
                .contains("nothing selected")
        );
        assert!(
            validate_term("two\nlines")
                .unwrap_err()
                .to_string()
                .contains("several lines")
        );
        assert!(
            validate_term("a b c d e f")
                .unwrap_err()
                .to_string()
                .contains("more than 5 words")
        );
        assert!(
            validate_term(&"x".repeat(50))
                .unwrap_err()
                .to_string()
                .contains("too long")
        );
        assert!(
            validate_term("bad {term}")
                .unwrap_err()
                .to_string()
                .contains("contains")
        );
    }
}
