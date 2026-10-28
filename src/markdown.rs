//! What a live Markdown editor needs to know about its text: which parts to
//! style, and how typing continues a list. The text itself stays Markdown.

use std::ops::Range;

use pulldown_cmark::{Event, Options, Parser, Tag};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Heading(u8),
    Strong,
    Emphasis,
    Strike,
    Code,
    CodeBlock,
    Link,
    Quote,
    /// Markup characters such as `**` or `#`, shown faintly.
    Syntax,
    /// A list bullet or number.
    Bullet,
    /// A task's `[ ]` or `[x]`, which can be clicked.
    Checkbox(bool),
}

/// A styled part of the text, in bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub range: Range<usize>,
    pub style: Style,
}

pub fn spans(text: &str) -> Vec<Span> {
    let mut out = Vec::new();
    let mut push = |range: Range<usize>, style| {
        if range.start < range.end && range.end <= text.len() {
            out.push(Span { range, style });
        }
    };
    let options =
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS | Options::ENABLE_TABLES;
    for (event, range) in Parser::new_ext(text, options).into_offset_iter() {
        let part = &text[range.clone()];
        let (start, end) = (range.start, range.end);
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                push(range.clone(), Style::Heading(level as u8));
                let indent = part.len() - part.trim_start().len();
                let hashes = part[indent..].len() - part[indent..].trim_start_matches('#').len();
                if hashes > 0 {
                    let after = &part[indent + hashes..];
                    let spaces = after.len() - after.trim_start_matches([' ', '\t']).len();
                    push(
                        start + indent..start + indent + hashes + spaces,
                        Style::Syntax,
                    );
                }
            }
            Event::Start(Tag::Strong) => delimited(&mut push, part, start, end, Style::Strong),
            Event::Start(Tag::Emphasis) => delimited(&mut push, part, start, end, Style::Emphasis),
            Event::Start(Tag::Strikethrough) => {
                delimited(&mut push, part, start, end, Style::Strike)
            }
            Event::Code(_) => delimited(&mut push, part, start, end, Style::Code),
            Event::Start(Tag::CodeBlock(_)) => {
                push(range.clone(), Style::CodeBlock);
                let fence = |line: &str| {
                    let line = line.trim_start();
                    line.starts_with("```") || line.starts_with("~~~")
                };
                if let Some(first) = part.lines().next().filter(|line| fence(line)) {
                    push(start..start + first.len(), Style::Syntax);
                }
                let body = part.trim_end_matches(['\n', '\r']);
                if let Some(at) = body.rfind('\n')
                    && fence(&body[at + 1..])
                {
                    push(start + at + 1..start + body.len(), Style::Syntax);
                }
            }
            Event::Start(Tag::Link { .. }) => {
                if part.starts_with('[')
                    && let Some(close) = part.rfind("](")
                {
                    push(start..start + 1, Style::Syntax);
                    push(start + 1..start + close, Style::Link);
                    push(start + close..end, Style::Syntax);
                } else {
                    push(range.clone(), Style::Link);
                }
            }
            Event::Start(Tag::BlockQuote(_)) => {
                push(range.clone(), Style::Quote);
                let mut at = start;
                for line in part.split_inclusive('\n') {
                    let indent = line.len() - line.trim_start().len();
                    if line[indent..].starts_with('>') {
                        push(at + indent..at + indent + 1, Style::Syntax);
                    }
                    at += line.len();
                }
            }
            Event::Start(Tag::Item) => {
                let indent = part.len() - part.trim_start().len();
                let marker = part[indent..]
                    .find(char::is_whitespace)
                    .unwrap_or(part.len() - indent);
                push(start + indent..start + indent + marker, Style::Bullet);
            }
            Event::TaskListMarker(done) => push(range.clone(), Style::Checkbox(done)),
            Event::Rule => push(range.clone(), Style::Syntax),
            _ => {}
        }
    }
    out
}

/// Styles a span like `**bold**`, with its delimiters faint.
fn delimited(
    push: &mut impl FnMut(Range<usize>, Style),
    part: &str,
    start: usize,
    end: usize,
    style: Style,
) {
    push(start..end, style);
    let Some(first) = part.chars().next() else {
        return;
    };
    let run = part.len() - part.trim_start_matches(first).len();
    let closing = part.len() - part.trim_end_matches(first).len();
    let width = run.min(closing).min(part.len() / 2);
    if width > 0 && matches!(first, '*' | '_' | '~' | '`') {
        push(start..start + width, Style::Syntax);
        push(end - width..end, Style::Syntax);
    }
}

/// What Enter does at the end of `line` (the text before the cursor).
#[derive(Debug, PartialEq, Eq)]
pub enum Enter {
    /// Start the next line with this: the same bullet, the next number, a
    /// new checkbox.
    Continue(String),
    /// The item is empty: drop its marker and end the list.
    End,
}

pub fn on_enter(line: &str) -> Option<Enter> {
    let (indent, marker, rest) = list_item(line)?;
    let (checkbox, content) = match rest
        .strip_prefix("[ ]")
        .or_else(|| rest.strip_prefix("[x]"))
        .or_else(|| rest.strip_prefix("[X]"))
    {
        Some(after) if after.is_empty() || after.starts_with(' ') => (true, after),
        _ => (false, rest),
    };
    if content.trim().is_empty() {
        return Some(Enter::End);
    }
    let next = match marker.strip_suffix(['.', ')']) {
        Some(number) => {
            let delimiter = &marker[number.len()..];
            format!("{}{delimiter}", number.parse::<u64>().ok()? + 1)
        }
        None => marker.to_string(),
    };
    let checkbox = if checkbox { "[ ] " } else { "" };
    Some(Enter::Continue(format!("{indent}{next} {checkbox}")))
}

/// Splits a list item or quote line into indent, marker and the rest.
pub fn list_item(line: &str) -> Option<(&str, &str, &str)> {
    let indent = &line[..line.len() - line.trim_start_matches([' ', '\t']).len()];
    let after = &line[indent.len()..];
    let marker_len = if after.starts_with(['-', '*', '+', '>']) {
        1
    } else {
        let digits = after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits == 0 || digits > 9 || !after[digits..].starts_with(['.', ')']) {
            return None;
        }
        digits + 1
    };
    let rest = &after[marker_len..];
    let rest = rest
        .strip_prefix(' ')
        .or_else(|| rest.is_empty().then_some(""))?;
    Some((indent, &after[..marker_len], rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styled(text: &str, style: Style) -> Vec<&str> {
        spans(text)
            .into_iter()
            .filter(|span| span.style == style)
            .map(|span| &text[span.range])
            .collect()
    }

    #[test]
    fn headings_and_inline_markup_keep_their_markup_faint() {
        let text = "## Plan\n\n**bold**, *it*, ~~old~~ and `code`\n";
        assert_eq!(styled(text, Style::Heading(2)), ["## Plan\n"]);
        assert_eq!(styled(text, Style::Strong), ["**bold**"]);
        assert_eq!(styled(text, Style::Emphasis), ["*it*"]);
        assert_eq!(styled(text, Style::Strike), ["~~old~~"]);
        assert_eq!(styled(text, Style::Code), ["`code`"]);
        assert_eq!(
            styled(text, Style::Syntax),
            ["## ", "**", "**", "*", "*", "~~", "~~", "`", "`"]
        );
    }

    #[test]
    fn lists_tasks_links_quotes_and_code_blocks() {
        let text = "- [ ] open\n- [x] done\n1. first\n\n> quote\n\n[soi](https://x)\n\n```rust\nfn x() {}\n```\n";
        assert_eq!(styled(text, Style::Checkbox(false)), ["[ ]"]);
        assert_eq!(styled(text, Style::Checkbox(true)), ["[x]"]);
        assert_eq!(styled(text, Style::Bullet), ["-", "-", "1."]);
        assert_eq!(styled(text, Style::Link), ["soi"]);
        assert_eq!(styled(text, Style::Quote), ["> quote\n"]);
        assert_eq!(styled(text, Style::CodeBlock), ["```rust\nfn x() {}\n```"]);
        let syntax = styled(text, Style::Syntax);
        for part in [">", "[", "](https://x)", "```rust", "```"] {
            assert!(syntax.contains(&part), "{part} in {syntax:?}");
        }
    }

    #[test]
    fn spans_stay_on_char_boundaries_with_unusual_text() {
        for text in [
            "",
            "*",
            "**",
            "# ",
            "ü **ä** `ö`",
            "- [ ]",
            "```\nunclosed",
            "> \n>",
        ] {
            for span in spans(text) {
                assert!(text.is_char_boundary(span.range.start), "{text:?}");
                assert!(text.is_char_boundary(span.range.end), "{text:?}");
            }
        }
    }

    #[test]
    fn enter_continues_lists_and_ends_empty_items() {
        let next = |line| on_enter(line);
        assert_eq!(next("- item"), Some(Enter::Continue("- ".into())));
        assert_eq!(next("  * nested"), Some(Enter::Continue("  * ".into())));
        assert_eq!(next("9. ninth"), Some(Enter::Continue("10. ".into())));
        assert_eq!(next("3) third"), Some(Enter::Continue("4) ".into())));
        assert_eq!(next("- [x] done"), Some(Enter::Continue("- [ ] ".into())));
        assert_eq!(next("> quoted"), Some(Enter::Continue("> ".into())));
        assert_eq!(next("- "), Some(Enter::End));
        assert_eq!(next("- [ ] "), Some(Enter::End));
        assert_eq!(next("1."), Some(Enter::End));
        assert_eq!(next("plain text"), None);
        assert_eq!(next("-not a list"), None);
        assert_eq!(next("2026 was a year"), None);
    }
}
