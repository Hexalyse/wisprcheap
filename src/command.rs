//! Command mode: a spoken instruction applied to the selected text (or writing new text).

use anyhow::{Result, bail};

use crate::config::{DictionaryEntry, LlmOptions};
use crate::llm::{ChatResult, chat_complete};
use crate::polish::dictionary_block;

const SYSTEM_PROMPT: &str = "You are a text assistant driven by voice. The user holds a hotkey and speaks an instruction; you receive its speech-to-text transcript (it may contain transcription errors, filler words or self-corrections: go by the intent).

If a <selection> is provided, apply the instruction to that text (rewrite, shorten, translate, fix, reformat, change the tone...). Your output replaces the selection.
If there is no selection, write the text the instruction asks for (a reply, a message, a list...). Your output is inserted at the cursor.

Rules:
- Return only the final text: no preamble, explanation, quotes or commentary.
- Keep the language of the selection unless the instruction asks for another language. Without a selection, write in the language of the instruction.
- Keep the selection's formatting (line breaks, lists, markdown, code) unless the instruction asks to change it.
- Change only what the instruction asks for.
- If the selection is code, return code only, without code fences unless the selection had them.";

pub fn user_message(instruction: &str, selection: Option<&str>) -> String {
    match selection {
        Some(sel) if !sel.is_empty() => {
            format!(
                "<instruction>\n{instruction}\n</instruction>\n\n<selection>\n{sel}\n</selection>"
            )
        }
        _ => format!("<instruction>\n{instruction}\n</instruction>\n\n(no selection)"),
    }
}

/// Unwrap a code fence around everything: "```lang\n...\n```" -> "...".
fn unwrap_fence(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("```")?;
    let rest = rest.trim_start_matches(|c: char| c.is_ascii_lowercase());
    let rest = rest.strip_prefix('\n')?;
    let inner = rest.strip_suffix("```")?;
    Some(inner.strip_suffix('\n').unwrap_or(inner))
}

pub fn clean_output(raw: &str, selection: Option<&str>) -> String {
    let mut text = raw.trim();
    if !selection.is_some_and(|s| s.contains("```"))
        && let Some(inner) = unwrap_fence(text)
    {
        text = inner;
    }
    if let Some(rest) = text.strip_prefix("<selection>") {
        text = rest.trim_start();
    }
    if let Some(rest) = text.strip_suffix("</selection>") {
        text = rest.trim_end();
    }
    text.to_string()
}

pub struct Commander {
    pub model: String,
    llm: LlmOptions,
    system: String,
}

impl Commander {
    pub fn new(llm: &LlmOptions, dictionary: &[DictionaryEntry]) -> Self {
        let block = dictionary_block(dictionary);
        let system = if block.is_empty() {
            SYSTEM_PROMPT.to_string()
        } else {
            format!("{SYSTEM_PROMPT}\n\n{block}")
        };
        Self {
            model: llm.model.clone(),
            llm: llm.clone(),
            system,
        }
    }

    pub async fn run(&self, instruction: &str, selection: Option<&str>) -> Result<ChatResult> {
        let user = user_message(instruction, selection);
        let result = chat_complete(&self.llm, &self.system, &user, "Command").await?;
        let text = clean_output(&result.text, selection);
        if text.trim().is_empty() {
            bail!("Command: empty response");
        }
        Ok(ChatResult { text, ..result })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages() {
        assert!(
            user_message("translate", Some("Hello.")).contains("<selection>\nHello.\n</selection>")
        );
        assert!(user_message("write hello", None).contains("(no selection)"));
    }

    #[test]
    fn unwraps_fences_unless_selection_had_them() {
        assert_eq!(clean_output("```js\nlet a;\n```", Some("let a")), "let a;");
        assert_eq!(
            clean_output("```js\nlet a;\n```", Some("```js\nlet a\n```")),
            "```js\nlet a;\n```"
        );
        assert_eq!(
            clean_output("<selection>Bonjour.</selection>", None),
            "Bonjour."
        );
    }
}
