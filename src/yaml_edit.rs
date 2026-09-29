//! Comment-preserving edits of the YAML config (sync write-back): set or remove a key, and add,
//! replace or remove list items. An edit changes as few lines as possible and is checked by parsing
//! the result: the new document must equal the old one with exactly that change, otherwise the edit
//! is refused and the text stays as it was.

use anyhow::{Result, anyhow, bail};
use serde_yaml::{Mapping, Value};

/// How list items that are mappings are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemStyle {
    /// `- term: pnpm` + `  soundsLike: [...]` on the next lines.
    Block,
    /// `- { from: fr, to: en }`.
    Flow,
}

#[derive(Debug, Clone, Copy)]
struct Line {
    start: usize,
    /// End of the content, without the line break.
    end: usize,
    /// Start of the next line.
    next: usize,
}

/// Byte ranges of the lines of `source`.
fn lines(source: &str) -> Vec<Line> {
    let mut out = Vec::new();
    let bytes = source.as_bytes();
    let mut start = 0;
    while start < source.len() {
        let next = source[start..]
            .find('\n')
            .map(|i| start + i + 1)
            .unwrap_or(source.len());
        let mut end = if bytes[next - 1] == b'\n' { next - 1 } else { next };
        if end > start && bytes[end - 1] == b'\r' {
            end -= 1;
        }
        out.push(Line { start, end, next });
        start = next;
    }
    out
}

fn indent_of(s: &str) -> usize {
    s.len() - s.trim_start_matches(' ').len()
}

/// Blank, comment-only or a document marker: ignored when working out the structure.
fn is_skip(s: &str) -> bool {
    let t = s.trim();
    t.is_empty() || t.starts_with('#') || t == "---"
}

fn is_dash_item(content: &str) -> bool {
    content == "-" || content.starts_with("- ") || content.starts_with("-\t")
}

/// `key:` at the start of `content` (the line after its indentation): the key and the offset just
/// after the colon.
fn parse_key(content: &str) -> Option<(String, usize)> {
    let (key, after_key) = if let Some(r) = content.strip_prefix('"') {
        let end = r.find('"')?;
        (r[..end].to_string(), end + 2)
    } else if let Some(r) = content.strip_prefix('\'') {
        let end = r.find('\'')?;
        (r[..end].to_string(), end + 2)
    } else {
        let len = content
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
            .unwrap_or(content.len());
        if len == 0 {
            return None;
        }
        (content[..len].to_string(), len)
    };
    let after = &content[after_key..];
    let spaces = after.len() - after.trim_start_matches([' ', '\t']).len();
    let colon = after[spaces..].strip_prefix(':')?;
    if !colon.is_empty() && !colon.starts_with([' ', '\t']) {
        return None;
    }
    Some((key, after_key + spaces + 1))
}

/// Splits what follows a key's colon into the value and a trailing comment (quote-aware): returns
/// (value start, value end, comment start), relative to `rest`.
fn split_value(rest: &str) -> (usize, usize, Option<usize>) {
    let bytes = rest.as_bytes();
    let start = rest.len() - rest.trim_start_matches([' ', '\t']).len();
    let mut i = start;
    let mut prev_ws = true;
    let mut token_start = true;
    let mut comment = None;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'#' && prev_ws {
            comment = Some(i);
            break;
        }
        if (c == b'"' || c == b'\'') && token_start {
            i += 1;
            while i < bytes.len() {
                if c == b'"' && bytes[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if bytes[i] == c {
                    if c == b'\'' && bytes.get(i + 1) == Some(&b'\'') {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            i += 1;
            prev_ws = false;
            token_start = false;
            continue;
        }
        prev_ws = c == b' ' || c == b'\t';
        token_start = prev_ws || matches!(c, b'[' | b'{' | b',' | b':');
        i += 1;
    }
    let region_end = comment.unwrap_or(rest.len()).min(rest.len());
    let end = start + rest[start.min(region_end)..region_end].trim_end_matches([' ', '\t']).len();
    (start, end.max(start), comment)
}

/// A key found in the text.
#[derive(Debug, Clone, Copy)]
struct Node {
    line: usize,
    indent: usize,
    /// Absolute offset just after the colon.
    after_colon: usize,
    /// Line index after the last content line of the value.
    end_line: usize,
}

enum Found {
    Node(Node),
    /// `path[depth..]` doesn't exist; `parent` is the deepest existing key (None: the root).
    Missing { depth: usize, parent: Option<Node> },
}

enum Scalar {
    Inline(String),
    /// Header (`|` or `|-`) and content lines.
    Literal(&'static str, Vec<String>),
}

fn probe_equals(probe: &str, expected: &Value, flow: bool) -> bool {
    match serde_yaml::from_str::<Value>(probe) {
        Ok(Value::Sequence(seq)) if flow => seq.len() == 1 && &seq[0] == expected,
        Ok(Value::Mapping(m)) if !flow => m.len() == 1 && m.get("k") == Some(expected),
        _ => false,
    }
}

fn quoted(s: &str) -> String {
    // JSON strings are valid YAML double-quoted scalars.
    serde_json::to_string(s).unwrap_or_else(|_| format!("\"{s}\""))
}

/// A scalar on one line: plain when that reads back identically, else double-quoted.
fn inline_scalar(value: &Value, flow: bool) -> Result<String> {
    Ok(match value {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(_) => serde_yaml::to_string(value)?.trim().to_string(),
        Value::String(s) => {
            let probe = if flow { format!("[{s}]") } else { format!("k: {s}") };
            if !s.contains(['\n', '\r']) && probe_equals(&probe, value, flow) {
                s.clone()
            } else {
                quoted(s)
            }
        }
        Value::Sequence(items) => {
            let parts: Result<Vec<String>> = items.iter().map(|v| inline_scalar(v, true)).collect();
            format!("[{}]", parts?.join(", "))
        }
        Value::Mapping(map) => {
            if map.is_empty() {
                return Ok("{}".into());
            }
            let mut parts = Vec::new();
            for (k, v) in map {
                let key = k.as_str().ok_or_else(|| anyhow!("non-string key"))?;
                parts.push(format!("{}: {}", render_key(key), inline_scalar(v, true)?));
            }
            format!("{{ {} }}", parts.join(", "))
        }
        Value::Tagged(_) => bail!("tagged values are not supported"),
    })
}

/// A value after `key:`: multi-line strings become literal blocks when possible.
fn block_scalar(value: &Value) -> Result<Scalar> {
    if let Value::String(s) = value
        && s.contains('\n')
        && !s.contains('\r')
        && !s.starts_with([' ', '\t', '\n'])
    {
        let (header, body) = if let Some(body) = s.strip_suffix('\n') {
            ("|", body)
        } else {
            ("|-", s.as_str())
        };
        if !body.ends_with('\n') {
            return Ok(Scalar::Literal(header, body.split('\n').map(String::from).collect()));
        }
    }
    Ok(Scalar::Inline(inline_scalar(value, false)?))
}

fn render_key(key: &str) -> String {
    if !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        key.to_string()
    } else {
        quoted(key)
    }
}

fn parse_doc(text: &str) -> Result<Value> {
    if text.trim().is_empty() {
        return Ok(Value::Mapping(Mapping::new()));
    }
    let v: Value = serde_yaml::from_str(text)?;
    Ok(if v.is_null() { Value::Mapping(Mapping::new()) } else { v })
}

/// The slot at `path`, creating the mappings on the way (and a null value at the end).
fn slot_mut<'a>(root: &'a mut Value, path: &[&str]) -> Result<&'a mut Value> {
    let mut cur = root;
    for key in path {
        if cur.is_null() {
            *cur = Value::Mapping(Mapping::new());
        }
        let map = cur
            .as_mapping_mut()
            .ok_or_else(|| anyhow!("{key}: the parent is not a mapping"))?;
        if !map.contains_key(*key) {
            map.insert(Value::String((*key).to_string()), Value::Null);
        }
        cur = map.get_mut(*key).expect("just inserted");
    }
    Ok(cur)
}

pub fn get<'a>(root: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut cur = root;
    for key in path {
        cur = cur.as_mapping()?.get(*key)?;
    }
    Some(cur)
}

/// The config text being edited.
pub struct YamlText {
    text: String,
    nl: &'static str,
}

impl YamlText {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
        Self { text, nl }
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn into_string(self) -> String {
        self.text
    }

    /// The parsed document (without `${VAR}` interpolation).
    pub fn doc(&self) -> Result<Value> {
        parse_doc(&self.text)
    }

    fn line_text(&self, l: Line) -> &str {
        &self.text[l.start..l.end]
    }

    /// Line index after the last content line of the value of the key on `key_line`.
    fn block_end(&self, ls: &[Line], key_line: usize, key_indent: usize, inline_empty: bool) -> usize {
        let mut last = key_line;
        let mut same_indent_list = false;
        let mut first = true;
        for (i, &l) in ls.iter().enumerate().skip(key_line + 1) {
            let s = self.line_text(l);
            if is_skip(s) {
                continue;
            }
            let ind = indent_of(s);
            let dash = is_dash_item(&s[ind..]);
            if ind > key_indent {
                last = i;
            } else if ind == key_indent && dash && inline_empty && (first || same_indent_list) {
                same_indent_list = true;
                last = i;
            } else {
                break;
            }
            first = false;
        }
        last + 1
    }

    fn node_at(&self, ls: &[Line], i: usize, indent: usize, after_colon_rel: usize) -> Node {
        let l = ls[i];
        let after_colon = l.start + indent + after_colon_rel;
        let rest = &self.text[after_colon..l.end];
        let (vs, ve, _) = split_value(rest);
        let inline_empty = vs == ve;
        Node {
            line: i,
            indent,
            after_colon,
            end_line: self.block_end(ls, i, indent, inline_empty),
        }
    }

    fn find(&self, path: &[&str]) -> Result<Found> {
        let ls = lines(&self.text);
        let mut range = 0..ls.len();
        let mut parent: Option<Node> = None;
        for (depth, key) in path.iter().enumerate() {
            let min_indent = parent.map(|p| p.indent + 1).unwrap_or(0);
            let first = range.clone().find(|&i| !is_skip(self.line_text(ls[i])));
            let Some(first) = first else {
                return Ok(Found::Missing { depth, parent });
            };
            let child_indent = indent_of(self.line_text(ls[first]));
            if child_indent < min_indent {
                return Ok(Found::Missing { depth, parent });
            }
            let mut hit = None;
            let mut i = range.start;
            while i < range.end {
                let s = self.line_text(ls[i]);
                if is_skip(s) || indent_of(s) != child_indent {
                    i += 1;
                    continue;
                }
                let content = &s[child_indent..];
                if is_dash_item(content) {
                    bail!("{}: expected a mapping, found a list", path[..depth].join("."));
                }
                match parse_key(content) {
                    Some((k, after)) => {
                        let node = self.node_at(&ls, i, child_indent, after);
                        if k == *key {
                            hit = Some(node);
                            break;
                        }
                        // Skip the value of this sibling (it may be a list at the same indent).
                        i = node.end_line.max(i + 1);
                    }
                    None => i += 1,
                }
            }
            let Some(node) = hit else {
                return Ok(Found::Missing { depth, parent });
            };
            range = node.line + 1..node.end_line;
            parent = Some(node);
        }
        Ok(Found::Node(parent.expect("non-empty path")))
    }

    /// The value of a key: (absolute value start, value end, comment start or line end).
    fn inline(&self, node: Node) -> (usize, usize, Option<usize>) {
        let ls = lines(&self.text);
        let end = ls[node.line].end;
        let (vs, ve, c) = split_value(&self.text[node.after_colon..end]);
        (
            node.after_colon + vs,
            node.after_colon + ve,
            c.map(|c| node.after_colon + c),
        )
    }

    fn inline_text(&self, node: Node) -> &str {
        let (vs, ve, _) = self.inline(node);
        &self.text[vs..ve]
    }

    /// Byte offset where lines can be inserted after line index `end_line - 1`, and whether a line
    /// break must be added first (last line without one).
    fn insert_point(&self, end_line: usize) -> (usize, bool) {
        let ls = lines(&self.text);
        if end_line == 0 || ls.is_empty() {
            return (0, false);
        }
        let l = ls[end_line - 1];
        (l.next, l.next == l.end)
    }

    fn checked(&mut self, candidate: String, expected: &Value) -> Result<()> {
        let got = parse_doc(&candidate).map_err(|e| anyhow!("the edit breaks the YAML: {e}"))?;
        if &got != expected {
            bail!("the edit doesn't read back as expected");
        }
        self.text = candidate;
        Ok(())
    }

    /// Lines for `keys` (nested) ending with `value`, the first key indented by `indent`.
    fn render_entry(&self, indent: usize, keys: &[&str], value: &Value, style: ItemStyle) -> Result<String> {
        let nl = self.nl;
        let mut out = String::new();
        for (i, k) in keys.iter().enumerate() {
            let ind = indent + 2 * i;
            let pad = " ".repeat(ind);
            let key = render_key(k);
            if i + 1 < keys.len() {
                out.push_str(&format!("{pad}{key}:{nl}"));
                continue;
            }
            match value {
                Value::Sequence(items) if !items.is_empty() => {
                    out.push_str(&format!("{pad}{key}:{nl}"));
                    for item in items {
                        out.push_str(&self.render_item(ind + 2, item, style)?);
                    }
                }
                _ => match block_scalar(value)? {
                    Scalar::Inline(s) => out.push_str(&format!("{pad}{key}: {s}{nl}")),
                    Scalar::Literal(header, body) => {
                        out.push_str(&format!("{pad}{key}: {header}{nl}"));
                        out.push_str(&self.literal_body(ind + 2, &body));
                    }
                },
            }
        }
        Ok(out)
    }

    fn literal_body(&self, indent: usize, body: &[String]) -> String {
        let pad = " ".repeat(indent);
        body.iter()
            .map(|l| {
                if l.is_empty() {
                    self.nl.to_string()
                } else {
                    format!("{pad}{l}{}", self.nl)
                }
            })
            .collect()
    }

    fn render_item(&self, indent: usize, item: &Value, style: ItemStyle) -> Result<String> {
        let nl = self.nl;
        let pad = " ".repeat(indent);
        match item {
            Value::Mapping(map) if style == ItemStyle::Block && !map.is_empty() => {
                let mut out = String::new();
                for (n, (k, v)) in map.iter().enumerate() {
                    let key = k.as_str().ok_or_else(|| anyhow!("non-string key"))?;
                    let lead = if n == 0 { format!("{pad}- ") } else { format!("{pad}  ") };
                    out.push_str(&format!("{lead}{}: {}{nl}", render_key(key), inline_scalar(v, true)?));
                }
                Ok(out)
            }
            Value::String(_) => Ok(format!("{pad}- {}{nl}", inline_scalar(item, false)?)),
            _ => Ok(format!("{pad}- {}{nl}", inline_scalar(item, true)?)),
        }
    }

    /// Adds `keys` + `value` under `parent` (None: at the end of the document).
    fn insert_under(&self, parent: Option<Node>, keys: &[&str], value: &Value, style: ItemStyle) -> Result<String> {
        let mut text = self.text.clone();
        let Some(p) = parent else {
            let mut out = text;
            if !out.is_empty() && !out.ends_with('\n') {
                out.push_str(self.nl);
            }
            out.push_str(&self.render_entry(0, keys, value, style)?);
            return Ok(out);
        };
        let ls = lines(&self.text);
        let (vs, ve, comment) = self.inline(p);
        let inline = self.text[vs..ve].trim();
        let mut shift: isize = 0;
        if !inline.is_empty() {
            if !matches!(inline, "null" | "~" | "{}") {
                bail!("{}: can't add keys to an inline value", keys[0]);
            }
            // `section: null  # c` -> `section:  # c`
            let line_end = ls[p.line].end;
            let replacement = match comment {
                Some(c) => format!(" {}", &self.text[c..line_end]),
                None => String::new(),
            };
            text.replace_range(p.after_colon..line_end, &replacement);
            shift = replacement.len() as isize - (line_end - p.after_colon) as isize;
        }
        let first_child = (p.line + 1..p.end_line).find(|&i| !is_skip(self.line_text(ls[i])));
        let child_indent = match first_child {
            Some(i) => {
                let s = self.line_text(ls[i]);
                if is_dash_item(&s[indent_of(s)..]) {
                    bail!("{}: the parent is a list", keys[0]);
                }
                indent_of(s)
            }
            None => p.indent + 2,
        };
        let (at, needs_nl) = self.insert_point(p.end_line);
        let at = (at as isize + shift) as usize;
        let mut chunk = String::new();
        if needs_nl {
            chunk.push_str(self.nl);
        }
        chunk.push_str(&self.render_entry(child_indent, keys, value, style)?);
        text.insert_str(at, &chunk);
        Ok(text)
    }

    /// Sets the value at `path` (creating the missing keys). Scalars, or small flow values.
    pub fn set(&mut self, path: &[&str], value: &Value) -> Result<()> {
        let mut expected = self.doc()?;
        *slot_mut(&mut expected, path)? = value.clone();
        let candidate = match self.find(path)? {
            Found::Missing { depth, parent } => {
                self.insert_under(parent, &path[depth..], value, ItemStyle::Flow)?
            }
            Found::Node(node) => self.replace_value(node, value)?,
        };
        self.checked(candidate, &expected)
            .map_err(|e| anyhow!("{}: {e}", path.join(".")))
    }

    fn replace_value(&self, node: Node, value: &Value) -> Result<String> {
        let ls = lines(&self.text);
        let (vs, ve, comment) = self.inline(node);
        let line = ls[node.line];
        let tail = match comment {
            Some(c) if ve == vs => format!(" {}", &self.text[c..line.end]),
            Some(_) => self.text[ve..line.end].to_string(),
            None => String::new(),
        };
        let head = &self.text[line.start..node.after_colon];
        let mut out = String::new();
        match block_scalar(value)? {
            Scalar::Inline(s) => out.push_str(&format!("{head} {s}{tail}{}", self.nl)),
            Scalar::Literal(header, body) => {
                out.push_str(&format!("{head} {header}{tail}{}", self.nl));
                out.push_str(&self.literal_body(node.indent + 2, &body));
            }
        }
        // Replace the key line and the old block value (if any).
        let block_end = if node.end_line > node.line + 1 {
            ls[node.end_line - 1].next
        } else {
            line.next
        };
        let mut text = self.text.clone();
        let keep_nl_missing = block_end == ls[node.end_line - 1].end && block_end == text.len();
        if keep_nl_missing {
            // The file didn't end with a line break: don't add one.
            let trimmed = out.trim_end_matches(['\r', '\n']).to_string();
            out = trimmed;
        }
        text.replace_range(line.start..block_end, &out);
        Ok(text)
    }

    /// Removes a key and its value (no-op when absent).
    pub fn remove(&mut self, path: &[&str]) -> Result<()> {
        let mut expected = self.doc()?;
        let (parent_path, last) = path.split_at(path.len() - 1);
        let exists = get(&expected, path).is_some();
        if !exists {
            return Ok(());
        }
        let parent = if parent_path.is_empty() {
            &mut expected
        } else {
            slot_mut(&mut expected, parent_path)?
        };
        let now_empty = match parent.as_mapping_mut() {
            Some(map) => {
                map.remove(last[0]);
                map.is_empty()
            }
            None => false,
        };
        if now_empty && !parent_path.is_empty() {
            *parent = Value::Null;
        }
        let Found::Node(node) = self.find(path)? else {
            bail!("{}: not found in the text", path.join("."));
        };
        let ls = lines(&self.text);
        let start = ls[node.line].start;
        let end = ls[node.end_line - 1].next;
        let mut text = self.text.clone();
        text.replace_range(start..end, "");
        self.checked(text, &expected)
            .map_err(|e| anyhow!("{}: {e}", path.join(".")))
    }

    /// Item spans of the block list under `node`: (first line, end line exclusive).
    fn item_spans(&self, node: Node) -> Result<Vec<(usize, usize)>> {
        let ls = lines(&self.text);
        let mut spans: Vec<(usize, usize)> = Vec::new();
        let mut item_indent = None;
        for (i, &l) in ls.iter().enumerate().take(node.end_line).skip(node.line + 1) {
            let s = self.line_text(l);
            if is_skip(s) {
                continue;
            }
            let ind = indent_of(s);
            let dash = is_dash_item(&s[ind..]);
            match item_indent {
                None if dash => item_indent = Some(ind),
                None => bail!("expected a list"),
                _ => {}
            }
            if Some(ind) == item_indent && dash {
                spans.push((i, i + 1));
            } else if let Some(last) = spans.last_mut() {
                last.1 = i + 1;
            }
        }
        Ok(spans)
    }

    fn list_indent(&self, node: Node) -> usize {
        let ls = lines(&self.text);
        (node.line + 1..node.end_line)
            .map(|i| self.line_text(ls[i]))
            .find(|s| !is_skip(s))
            .map(indent_of)
            .unwrap_or(node.indent + 2)
    }

    /// Rewrites the list at `node` as a whole: flow lists stay flow (on one line), empty or null
    /// values become block lists.
    fn rewrite_inline_list(&self, node: Node, items: &[Value], style: ItemStyle) -> Result<String> {
        let ls = lines(&self.text);
        let (vs, ve, _) = self.inline(node);
        let inline = self.text[vs..ve].trim();
        let line = ls[node.line];
        let mut text = self.text.clone();
        let was_flow_items = inline.starts_with('[') && inline != "[]";
        if inline.starts_with('[') && !inline.ends_with(']') {
            bail!("multi-line flow lists are not supported");
        }
        if was_flow_items || items.is_empty() {
            let rendered: Result<Vec<String>> = items.iter().map(|v| inline_scalar(v, true)).collect();
            let flow = format!("[{}]", rendered?.join(", "));
            if inline.is_empty() {
                text.insert_str(node.after_colon, &format!(" {flow}"));
            } else {
                text.replace_range(vs..ve, &flow);
            }
            return Ok(text);
        }
        // Block list after the key line.
        let mut body = String::new();
        let item_indent = node.indent + 2;
        for item in items {
            body.push_str(&self.render_item(item_indent, item, style)?);
        }
        let (_, _, comment) = self.inline(node);
        let comment_tail = comment
            .map(|c| format!(" {}", &self.text[c..line.end]))
            .unwrap_or_default();
        let head = format!("{}{comment_tail}{}", &self.text[line.start..node.after_colon], self.nl);
        text.replace_range(line.start..line.next, &format!("{head}{body}"));
        if line.next == line.end && text.ends_with(self.nl) {
            // The file didn't end with a line break: keep it that way.
            let len = text.len() - self.nl.len();
            text.truncate(len);
        }
        Ok(text)
    }

    /// Replaces the first item matching `is_match` with `item`, or appends it.
    pub fn list_upsert(
        &mut self,
        path: &[&str],
        is_match: &dyn Fn(&Value) -> bool,
        item: Value,
        style: ItemStyle,
    ) -> Result<()> {
        let mut expected = self.doc()?;
        let slot = slot_mut(&mut expected, path)?;
        if slot.is_null() {
            *slot = Value::Sequence(Vec::new());
        }
        let seq = slot
            .as_sequence_mut()
            .ok_or_else(|| anyhow!("{} is not a list", path.join(".")))?;
        let before = seq.clone();
        let index = seq.iter().position(is_match);
        match index {
            Some(i) => seq[i] = item.clone(),
            None => seq.push(item.clone()),
        }
        let after = seq.clone();
        let candidate = match self.find(path)? {
            Found::Missing { depth, parent } => {
                self.insert_under(parent, &path[depth..], &Value::Sequence(vec![item]), style)?
            }
            Found::Node(node) => {
                let inline = self.inline_text(node).to_string();
                let has_block = node.end_line > node.line + 1;
                if !inline.is_empty() || !has_block {
                    self.rewrite_inline_list(node, &after, style)?
                } else {
                    let spans = self.item_spans(node)?;
                    if spans.len() != before.len() {
                        bail!("{}: can't match the list items", path.join("."));
                    }
                    let indent = self.list_indent(node);
                    let rendered = self.render_item(indent, &item, style)?;
                    let ls = lines(&self.text);
                    let mut text = self.text.clone();
                    match index {
                        Some(i) => {
                            let (a, b) = spans[i];
                            text.replace_range(ls[a].start..ls[b - 1].next, &rendered);
                            if ls[b - 1].next == ls[b - 1].end && text.ends_with(self.nl) {
                                let len = text.len() - self.nl.len();
                                text.truncate(len);
                            }
                        }
                        None => {
                            let end_line = spans.last().map(|s| s.1).unwrap_or(node.line + 1);
                            let (at, needs_nl) = self.insert_point(end_line);
                            let chunk = if needs_nl { format!("{}{rendered}", self.nl) } else { rendered };
                            text.insert_str(at, &chunk);
                        }
                    }
                    text
                }
            }
        };
        self.checked(candidate, &expected)
            .map_err(|e| anyhow!("{}: {e}", path.join(".")))
    }

    /// Removes every item matching `is_match` (no-op when there's none).
    pub fn list_remove(&mut self, path: &[&str], is_match: &dyn Fn(&Value) -> bool) -> Result<()> {
        let mut expected = self.doc()?;
        let Some(seq) = get(&expected, path).and_then(Value::as_sequence) else {
            return Ok(());
        };
        let before = seq.clone();
        let remove: Vec<usize> = before
            .iter()
            .enumerate()
            .filter(|(_, v)| is_match(v))
            .map(|(i, _)| i)
            .collect();
        if remove.is_empty() {
            return Ok(());
        }
        let after: Vec<Value> = before.iter().filter(|v| !is_match(v)).cloned().collect();
        *slot_mut(&mut expected, path)? = Value::Sequence(after.clone());
        let Found::Node(node) = self.find(path)? else {
            bail!("{}: not found in the text", path.join("."));
        };
        let inline = self.inline_text(node).to_string();
        let candidate = if !inline.is_empty() {
            self.rewrite_inline_list(node, &after, ItemStyle::Flow)?
        } else {
            let spans = self.item_spans(node)?;
            if spans.len() != before.len() {
                bail!("{}: can't match the list items", path.join("."));
            }
            let ls = lines(&self.text);
            let mut text = self.text.clone();
            for &i in remove.iter().rev() {
                let (a, b) = spans[i];
                text.replace_range(ls[a].start..ls[b - 1].next, "");
            }
            if after.is_empty() {
                // `key:` alone would read as null: make it an empty list (`key: []  # comment`).
                let tmp = YamlText { text, nl: self.nl };
                let Found::Node(n) = tmp.find(path)? else {
                    bail!("{}: lost the key", path.join("."));
                };
                let mut t = tmp.text;
                t.insert_str(n.after_colon, " []");
                t
            } else {
                text
            }
        };
        self.checked(candidate, &expected)
            .map_err(|e| anyhow!("{}: {e}", path.join(".")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Value {
        Value::String(v.into())
    }

    const CONFIG: &str = "# my config
transcription:
  provider: elevenlabs     # elevenlabs | openai
  elevenlabs:
    apiKey: ${ELEVENLABS_API_KEY}
    model: scribe_v2

polish:
  model: gpt-6-luna
  instructions: |
    Line one.
    Line two.
  timeoutMs: 10000

# Terms
dictionary:
  - git   # vcs
  - term: pnpm
    soundsLike: [p n p m]

command:
  # model: x
  timeoutMs: 30000
";

    #[test]
    fn set_keeps_comments_and_layout() {
        let mut y = YamlText::new(CONFIG);
        y.set(&["transcription", "provider"], &s("openai")).unwrap();
        assert!(y.as_str().contains("  provider: openai     # elevenlabs | openai\n"), "{}", y.as_str());
        y.set(&["polish", "model"], &s("gpt-6-sol")).unwrap();
        y.set(&["polish", "timeoutMs"], &Value::from(5000)).unwrap();
        assert_eq!(
            y.as_str(),
            CONFIG
                .replace("provider: elevenlabs", "provider: openai")
                .replace("model: gpt-6-luna", "model: gpt-6-sol")
                .replace("timeoutMs: 10000", "timeoutMs: 5000")
        );
    }

    #[test]
    fn set_replaces_block_scalars_and_writes_literals() {
        let mut y = YamlText::new(CONFIG);
        y.set(&["polish", "instructions"], &s("Be brief.")).unwrap();
        assert!(y.as_str().contains("  instructions: Be brief.\n  timeoutMs: 10000\n"), "{}", y.as_str());
        y.set(&["polish", "instructions"], &s("A: b\nsecond line\n")).unwrap();
        assert!(y.as_str().contains("  instructions: |\n    A: b\n    second line\n  timeoutMs"), "{}", y.as_str());
        y.set(&["polish", "instructions"], &s("no newline\nat end")).unwrap();
        assert!(y.as_str().contains("  instructions: |-\n    no newline\n    at end\n"), "{}", y.as_str());
    }

    #[test]
    fn set_creates_missing_keys_and_sections() {
        let mut y = YamlText::new(CONFIG);
        y.set(&["command", "model"], &s("gpt-6-sol")).unwrap();
        assert!(y.as_str().contains("  timeoutMs: 30000\n  model: gpt-6-sol\n"), "{}", y.as_str());
        y.set(&["transcription", "openai", "baseUrl"], &s("https://x/v1")).unwrap();
        assert!(y.as_str().contains("    model: scribe_v2\n  openai:\n    baseUrl: https://x/v1\n"), "{}", y.as_str());
        y.set(&["sync", "server"], &s("https://sync.example.com")).unwrap();
        assert!(y.as_str().ends_with("sync:\n  server: https://sync.example.com\n"), "{}", y.as_str());
        y.set(&["polish", "temperature"], &Value::Null).unwrap();
        assert!(y.as_str().contains("  timeoutMs: 10000\n  temperature: null\n"), "{}", y.as_str());
        // Values that need quoting.
        y.set(&["transcription", "language"], &s("true")).unwrap();
        y.set(&["transcription", "openai", "prompt"], &s("")).unwrap();
        let doc = y.doc().unwrap();
        assert_eq!(get(&doc, &["transcription", "language"]), Some(&s("true")));
        assert_eq!(get(&doc, &["transcription", "openai", "prompt"]), Some(&s("")));
    }

    #[test]
    fn null_and_commented_sections() {
        let mut y = YamlText::new("command:\n  # model: x\nsounds: {}\nhistory: null  # off\n");
        y.set(&["command", "model"], &s("m")).unwrap();
        y.set(&["sounds", "volume"], &Value::from(0.5)).unwrap();
        y.set(&["history", "enabled"], &Value::Bool(false)).unwrap();
        assert_eq!(
            y.as_str(),
            "command:\n  model: m\n  # model: x\nsounds:\n  volume: 0.5\nhistory: # off\n  enabled: false\n"
        );
    }

    #[test]
    fn remove_keys() {
        let mut y = YamlText::new(CONFIG);
        y.remove(&["polish", "instructions"]).unwrap();
        assert!(y.as_str().contains("  model: gpt-6-luna\n  timeoutMs: 10000\n"), "{}", y.as_str());
        y.remove(&["command", "timeoutMs"]).unwrap();
        assert!(y.as_str().ends_with("command:\n  # model: x\n"), "{}", y.as_str());
        y.remove(&["nothing", "here"]).unwrap();
    }

    #[test]
    fn crlf_and_no_final_newline() {
        let mut y = YamlText::new("polish:\r\n  model: a\r\n  enabled: true");
        y.set(&["polish", "enabled"], &Value::Bool(false)).unwrap();
        assert_eq!(y.as_str(), "polish:\r\n  model: a\r\n  enabled: false");
        y.set(&["polish", "minWords"], &Value::from(3)).unwrap();
        assert_eq!(y.as_str(), "polish:\r\n  model: a\r\n  enabled: false\r\n  minWords: 3\r\n");
    }

    fn term_is(t: &'static str) -> impl Fn(&Value) -> bool {
        move |v: &Value| {
            let term = v.as_str().or_else(|| v.get("term").and_then(Value::as_str));
            term.is_some_and(|x| x.eq_ignore_ascii_case(t))
        }
    }

    fn dict_item(term: &str, sounds: &[&str]) -> Value {
        if sounds.is_empty() {
            return s(term);
        }
        let mut m = Mapping::new();
        m.insert(s("term"), s(term));
        m.insert(s("soundsLike"), Value::Sequence(sounds.iter().map(|x| s(x)).collect()));
        Value::Mapping(m)
    }

    #[test]
    fn block_list_items() {
        let mut y = YamlText::new(CONFIG);
        y.list_upsert(&["dictionary"], &term_is("kubernetes"), s("Kubernetes"), ItemStyle::Block)
            .unwrap();
        assert!(y.as_str().contains("    soundsLike: [p n p m]\n  - Kubernetes\n\ncommand:"), "{}", y.as_str());
        y.list_upsert(&["dictionary"], &term_is("GIT"), dict_item("Git", &["get"]), ItemStyle::Block)
            .unwrap();
        assert!(y.as_str().contains("dictionary:\n  - term: Git\n    soundsLike: [get]\n  - term: pnpm\n"), "{}", y.as_str());
        y.list_remove(&["dictionary"], &term_is("pnpm")).unwrap();
        assert!(y.as_str().contains("  - term: Git\n    soundsLike: [get]\n  - Kubernetes\n"), "{}", y.as_str());
        y.list_remove(&["dictionary"], &|_| true).unwrap();
        assert!(y.as_str().contains("dictionary: []\n\ncommand:"), "{}", y.as_str());
        y.list_upsert(&["dictionary"], &term_is("a"), s("a"), ItemStyle::Block).unwrap();
        assert!(y.as_str().contains("dictionary:\n  - a\n\ncommand:"), "{}", y.as_str());
    }

    #[test]
    fn flow_same_indent_and_missing_lists() {
        let mut y = YamlText::new("dictionary: [git, Codex]  # inline\n");
        y.list_upsert(&["dictionary"], &term_is("pnpm"), s("pnpm"), ItemStyle::Block).unwrap();
        assert_eq!(y.as_str(), "dictionary: [git, Codex, pnpm]  # inline\n");
        y.list_remove(&["dictionary"], &term_is("codex")).unwrap();
        assert_eq!(y.as_str(), "dictionary: [git, pnpm]  # inline\n");

        let mut y = YamlText::new("dictionary:\n- a\n- b\nx: 1\n");
        y.list_upsert(&["dictionary"], &term_is("c"), s("c"), ItemStyle::Block).unwrap();
        assert_eq!(y.as_str(), "dictionary:\n- a\n- b\n- c\nx: 1\n");
        y.list_remove(&["dictionary"], &term_is("a")).unwrap();
        assert_eq!(y.as_str(), "dictionary:\n- b\n- c\nx: 1\n");

        let pair = |from: &str, to: &str| {
            let mut m = Mapping::new();
            m.insert(s("from"), s(from));
            m.insert(s("to"), s(to));
            Value::Mapping(m)
        };
        let mut y = YamlText::new("translation:\n  pairs: []\n  #  - { from: fr, to: en }\n  timeoutMs: 15000\n");
        y.list_upsert(&["translation", "pairs"], &|_| false, pair("fr", "en"), ItemStyle::Flow)
            .unwrap();
        assert_eq!(
            y.as_str(),
            "translation:\n  pairs:\n    - { from: fr, to: en }\n  #  - { from: fr, to: en }\n  timeoutMs: 15000\n"
        );
        let mut y = YamlText::new("hotkey:\n  keys: [Ctrl]\n");
        y.list_upsert(&["pricing", "overrides"], &|_| false, pair("a", "b"), ItemStyle::Flow)
            .unwrap();
        assert_eq!(y.as_str(), "hotkey:\n  keys: [Ctrl]\npricing:\n  overrides:\n    - { from: a, to: b }\n");
    }

    #[test]
    fn refuses_what_it_cant_edit_safely() {
        let mut y = YamlText::new("polish: {model: a}\n");
        assert!(y.set(&["polish", "enabled"], &Value::Bool(true)).is_err());
        assert_eq!(y.as_str(), "polish: {model: a}\n");
        let mut y = YamlText::new("dictionary:\n  - a\n  - [weird,\n     multi]\n");
        let r = y.list_upsert(&["dictionary"], &|_| false, s("b"), ItemStyle::Block);
        // Either handled correctly or refused, never a broken file.
        if r.is_ok() {
            assert!(y.doc().unwrap().get("dictionary").unwrap().as_sequence().unwrap().len() == 3);
        }
    }
}
