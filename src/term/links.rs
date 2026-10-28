//! Links in terminal output: finding URLs in text, and deciding whether a
//! link may be opened.
//!
//! Output is untrusted (a file you `cat`, a server you `ssh` into), so links
//! only open on Ctrl+click, the real target is shown before that, and only
//! harmless kinds of targets are opened: web and mail links, and local
//! folders and non-executable files.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

/// Longest link accepted, from text or from OSC 8.
pub const MAX_URL_LEN: usize = 2048;

const SCHEMES: [&str; 4] = ["https://", "http://", "mailto:", "file://"];

fn is_url_char(c: char) -> bool {
    !c.is_whitespace()
        && !c.is_control()
        && !matches!(c, '<' | '>' | '"' | '`' | '{' | '}' | '|' | '\\' | '^')
}

/// Finds a URL in `chars` that covers index `at`; returns its range.
pub fn find_url(chars: &[char], at: usize) -> Option<(usize, usize)> {
    if !chars.get(at).is_some_and(|&c| is_url_char(c)) {
        return None;
    }
    let mut start = at;
    while start > 0 && is_url_char(chars[start - 1]) {
        start -= 1;
    }
    let mut end = at + 1;
    while end < chars.len() && is_url_char(chars[end]) {
        end += 1;
    }
    // The URL begins at a scheme somewhere in the word, e.g. "(https://…".
    let word: String = chars[start..end].iter().collect();
    let (byte_offset, scheme) = SCHEMES
        .iter()
        .filter_map(|scheme| word.find(scheme).map(|i| (i, scheme)))
        .min_by_key(|(i, _)| *i)?;
    let url_start = start + word[..byte_offset].chars().count();

    // Leave out punctuation that ends a sentence, and closing brackets
    // that belong to the surrounding text.
    let mut url_end = end;
    while url_end > url_start {
        let url = &chars[url_start..url_end];
        let count = |c: char| url.iter().filter(|&&x| x == c).count();
        let unbalanced = |open: char, close: char| count(close) > count(open);
        match chars[url_end - 1] {
            '.' | ',' | ':' | ';' | '!' | '?' | '\'' => url_end -= 1,
            ')' if unbalanced('(', ')') => url_end -= 1,
            ']' if unbalanced('[', ']') => url_end -= 1,
            _ => break,
        }
    }
    let long_enough = url_end - url_start > scheme.chars().count();
    let fits = url_end - url_start <= MAX_URL_LEN;
    (long_enough && fits && (url_start..url_end).contains(&at)).then_some((url_start, url_end))
}

/// Whether a link may be opened; the error says why not.
pub fn check(uri: &str) -> Result<(), &'static str> {
    if uri.len() > MAX_URL_LEN || uri.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("the link contains invalid characters");
    }
    let (scheme, rest) = uri.split_once(':').ok_or("this is not a link")?;
    match scheme.to_ascii_lowercase().as_str() {
        "http" | "https" if rest.len() > 2 && rest.starts_with("//") => Ok(()),
        "http" | "https" => Err("malformed web link"),
        "mailto" => Ok(()),
        "file" => check_file(rest),
        _ => Err("links of this kind are not opened"),
    }
}

fn check_file(rest: &str) -> Result<(), &'static str> {
    const MALFORMED: &str = "malformed file link";
    let rest = rest.strip_prefix("//").ok_or(MALFORMED)?;
    let (host, path) = rest.split_at(rest.find('/').ok_or(MALFORMED)?);
    let local = host.is_empty() || host == "localhost" || Some(host) == hostname().as_deref();
    if !local {
        return Err("file links to other machines are not opened");
    }
    let path = PathBuf::from(percent_decode(path).ok_or(MALFORMED)?);
    let meta = std::fs::metadata(&path).map_err(|_| "the file does not exist")?;
    if meta.is_dir() {
        return Ok(());
    }
    if !meta.is_file() {
        return Err("only files and folders are opened");
    }
    if meta.permissions().mode() & 0o111 != 0 {
        return Err("executable files are not opened");
    }
    if path.extension().is_some_and(|e| e == "desktop") {
        return Err("launcher files are not opened");
    }
    Ok(())
}

fn hostname() -> Option<String> {
    crate::platform::current().hostname()
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url_in(text: &str, at: usize) -> Option<String> {
        let chars: Vec<char> = text.chars().collect();
        find_url(&chars, at).map(|(s, e)| chars[s..e].iter().collect())
    }

    #[test]
    fn finds_urls_in_text() {
        let text = "see (https://example.com/a_(b)) and http://x.io/p?q=1.";
        assert_eq!(
            url_in(text, 10).as_deref(),
            Some("https://example.com/a_(b)")
        );
        assert_eq!(
            url_in(text, 4),
            None,
            "the bracket before the URL isn't part of it"
        );
        assert_eq!(url_in(text, 45).as_deref(), Some("http://x.io/p?q=1"));
        assert_eq!(url_in("plain words", 3), None);
        assert_eq!(url_in("https://", 2), None, "a scheme alone is no link");
    }

    #[test]
    fn opens_only_harmless_links() {
        assert!(check("https://example.com").is_ok());
        assert!(check("mailto:me@example.com").is_ok());
        assert!(check("javascript:alert(1)").is_err());
        assert!(check("ssh://host").is_err());
        assert!(check("https://example.com/\x1b[31m").is_err());
        assert!(check("file://elsewhere.example/etc/hosts").is_err());
        assert!(check("file:///etc").is_ok(), "folders are fine");
        assert!(check("file:///usr/bin/ls").is_err(), "executables are not");
        assert!(check("file:///nonexistent%20file").is_err());
    }

    #[test]
    fn decodes_percent_escapes() {
        assert_eq!(percent_decode("/a%20b").as_deref(), Some("/a b"));
        assert_eq!(percent_decode("/bad%2"), None);
    }
}
