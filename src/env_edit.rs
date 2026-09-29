//! Setting one variable in a `.env` file (sync write-back of API keys): the matching line is
//! replaced, or a line is appended; every other line stays as it was.

use std::path::Path;

use anyhow::{Result, anyhow, bail};

fn name_of(line: &str) -> Option<&str> {
    let t = line.trim_start();
    let t = t.strip_prefix("export ").map(str::trim_start).unwrap_or(t);
    let end = t.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))?;
    let rest = t[end..].trim_start();
    rest.starts_with('=').then_some(&t[..end])
}

/// `NAME=value`, quoted only when needed (single quotes keep the value literal).
fn assignment(name: &str, value: &str) -> Result<String> {
    if value.contains(['\n', '\r']) {
        bail!("{name}: multi-line values are not supported");
    }
    let plain = value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_.:/+=@,~%".contains(c));
    Ok(if plain {
        format!("{name}={value}")
    } else if !value.contains('\'') {
        format!("{name}='{value}'")
    } else {
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"").replace('$', "\\$");
        format!("{name}=\"{escaped}\"")
    })
}

fn parse(text: &str) -> Result<Vec<(String, String)>> {
    dotenvy::from_read_iter(text.as_bytes())
        .map(|r| r.map_err(|e| anyhow!("{e}")))
        .collect()
}

/// The text with `name` set to `value`.
pub fn set_in_text(text: &str, name: &str, value: &str) -> Result<String> {
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let line = assignment(name, value)?;
    let mut out = String::with_capacity(text.len() + line.len() + 2);
    let mut replaced = false;
    for raw in text.split_inclusive('\n') {
        let content = raw.trim_end_matches(['\r', '\n']);
        if name_of(content) == Some(name) {
            if !replaced {
                out.push_str(&line);
                out.push_str(&raw[content.len()..]);
                replaced = true;
            }
            // Later duplicates would override the new value: drop them.
            continue;
        }
        out.push_str(raw);
    }
    if !replaced {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(nl);
        }
        out.push_str(&line);
        out.push_str(nl);
    }
    // Check that it reads back, and that nothing else changed.
    let before = parse(text).unwrap_or_default();
    let after = parse(&out).map_err(|e| anyhow!("the edit breaks the file: {e}"))?;
    let got = after.iter().rev().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
    if got != Some(value) {
        bail!("{name} doesn't read back as expected");
    }
    let others = |list: &[(String, String)]| -> Vec<(String, String)> {
        list.iter().filter(|(k, _)| k != name).cloned().collect()
    };
    if others(&before) != others(&after) {
        bail!("the edit changes other variables");
    }
    Ok(out)
}

/// Sets `name` in the `.env` file (created if missing, readable by the user only on Unix).
pub fn set_var(file: &Path, name: &str, value: &str) -> Result<()> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => bail!("can't read {}: {e}", file.display()),
    };
    let updated = set_in_text(&text, name, value).map_err(|e| anyhow!("{}: {e}", file.display()))?;
    if updated == text {
        return Ok(());
    }
    let existed = file.exists();
    std::fs::write(file, updated).map_err(|e| anyhow!("can't write {}: {e}", file.display()))?;
    #[cfg(unix)]
    if !existed {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = existed;
    Ok(())
}

/// Whether the file defines `name`.
pub fn defines(file: &Path, name: &str) -> bool {
    std::fs::read_to_string(file)
        .map(|t| t.lines().any(|l| name_of(l) == Some(name)))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_or_appends() {
        let text = "# keys\nOPENAI_API_KEY=old # comment\nexport ELEVENLABS_API_KEY=el\n";
        assert_eq!(
            set_in_text(text, "OPENAI_API_KEY", "sk-new").unwrap(),
            "# keys\nOPENAI_API_KEY=sk-new\nexport ELEVENLABS_API_KEY=el\n"
        );
        assert_eq!(
            set_in_text(text, "WISPRCHEAP_POLISH_API_KEY", "gsk_1").unwrap(),
            format!("{text}WISPRCHEAP_POLISH_API_KEY=gsk_1\n")
        );
        assert_eq!(set_in_text("", "A", "b").unwrap(), "A=b\n");
        assert_eq!(set_in_text("X=1\r\nA=a\r\nA=dup", "A", "c").unwrap(), "X=1\r\nA=c\r\n");
        assert_eq!(set_in_text("X=1", "A", "c").unwrap(), "X=1\nA=c\n");
    }

    #[test]
    fn quotes_when_needed() {
        let out = set_in_text("", "K", "a b#c").unwrap();
        assert_eq!(out, "K='a b#c'\n");
        let out = set_in_text("", "K", "it's $HOME").unwrap();
        assert_eq!(parse(&out).unwrap(), vec![("K".to_string(), "it's $HOME".to_string())]);
        assert!(set_in_text("", "K", "two\nlines").is_err());
    }
}
