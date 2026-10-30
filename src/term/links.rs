//! Links in terminal output: finding URLs in text, and deciding whether a
//! link may be opened.
//!
//! Output is untrusted (a file you `cat`, a server you `ssh` into), so links
//! require deliberate activation (Cmd+click on macOS, Ctrl+click on Linux,
//! or browse mode's open-link action), the real target is shown first, and only
//! harmless kinds of targets are opened: web and mail links, and local
//! folders and non-executable files.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

/// Longest link accepted, from text or from OSC 8.
pub const MAX_URL_LEN: usize = 2048;

const SCHEMES: [&str; 4] = ["https://", "http://", "mailto:", "file://"];

/// Extensions of files that start programs or open other targets when
/// opened, even when not executable.
const LAUNCHERS: [&str; 13] = [
    "desktop", "app", "command", "tool", "terminal", "fileloc", "webloc", "inetloc", "workflow",
    "prefpane", "pkg", "mpkg", "jar",
];

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
    // Count once: recounting for each trailing bracket is quadratic on
    // untrusted output and runs while the pointer moves over the terminal.
    let (mut round, mut square) = (0i64, 0i64);
    for c in &chars[url_start..end] {
        match c {
            '(' => round += 1,
            ')' => round -= 1,
            '[' => square += 1,
            ']' => square -= 1,
            _ => {}
        }
    }
    let mut url_end = end;
    while url_end > url_start {
        match chars[url_end - 1] {
            '.' | ',' | ':' | ';' | '!' | '?' | '\'' => url_end -= 1,
            ')' if round < 0 => {
                round += 1;
                url_end -= 1;
            }
            ']' if square < 0 => {
                square += 1;
                url_end -= 1;
            }
            _ => break,
        }
    }
    let long_enough = url_end - url_start > scheme.chars().count();
    let fits = chars[url_start..url_end]
        .iter()
        .map(|c| c.len_utf8())
        .sum::<usize>()
        <= MAX_URL_LEN;
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
    // Queries/fragments are interpreted by the URI launcher, not by the
    // filesystem. Never validate a different path from the one it opens.
    if path.contains(['?', '#']) {
        return Err(MALFORMED);
    }
    let decoded = percent_decode(path).ok_or(MALFORMED)?;
    if decoded.chars().any(char::is_control) {
        return Err(MALFORMED);
    }
    let requested = PathBuf::from(decoded);
    let path = std::fs::canonicalize(&requested).map_err(|_| "the file does not exist")?;
    let meta = std::fs::metadata(&path).map_err(|_| "the file does not exist")?;
    // Inspect both names: launchers may use the requested name's extension,
    // while symlinks can hide the target's. macOS app bundles execute too,
    // even though they are directories rather than executable regular files;
    // Terminal settings run their command, and location files open another
    // target unchecked.
    if [&requested, &path].iter().any(|path| {
        path.extension().is_some_and(|e| {
            LAUNCHERS
                .iter()
                .any(|launcher| e.eq_ignore_ascii_case(launcher))
        })
    }) {
        return Err("launcher files are not opened");
    }
    if meta.is_dir() {
        return Ok(());
    }
    if !meta.is_file() {
        return Err("only files and folders are opened");
    }
    if meta.permissions().mode() & 0o111 != 0 {
        return Err("executable files are not opened");
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
    fn url_lengths_use_bytes_and_trim_surrounding_brackets() {
        let short = "https://example.com/é";
        assert_eq!(url_in(short, 10).as_deref(), Some(short));
        assert!(check(short).is_ok());
        let text = format!("https://example.com/{}", "é".repeat(MAX_URL_LEN / 2));
        assert!(url_in(&text, 10).is_none(), "must match check's byte limit");
        assert_eq!(
            url_in("(https://example.com/a_(b))])", 10).as_deref(),
            Some("https://example.com/a_(b)")
        );
    }

    #[test]
    fn long_punctuation_suffix_is_trimmed() {
        let text = format!("https://example.com/{}", ")".repeat(30_000));
        assert_eq!(url_in(&text, 10).as_deref(), Some("https://example.com/"));
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
    fn rejects_disguised_launchers_and_ambiguous_file_uris() {
        let dir = std::env::temp_dir().join(format!("soi-links-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let launcher = dir.join("evil.desktop");
        std::fs::write(&launcher, "[Desktop Entry]\nType=Application\nExec=sh\n").unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&launcher, dir.join("readme.txt")).unwrap();
        std::fs::create_dir(dir.join("Evil.APP")).unwrap();
        std::fs::write(dir.join("run.terminal"), "plist").unwrap();
        std::fs::write(dir.join("elsewhere.fileloc"), "plist").unwrap();
        std::fs::write(dir.join("safe.txt"), "text").unwrap();
        std::os::unix::fs::symlink(dir.join("safe.txt"), dir.join("alias.desktop")).unwrap();
        std::fs::write(dir.join("safe.txt#fragment"), "text").unwrap();
        std::fs::write(dir.join("safe.txt?query"), "text").unwrap();
        for name in [
            "evil.desktop",
            "readme.txt",
            "alias.desktop",
            "Evil.APP",
            "run.terminal",
            "elsewhere.fileloc",
            "safe.txt#fragment",
            "safe.txt?query",
        ] {
            assert!(
                check(&format!("file://{}/{name}", dir.display())).is_err(),
                "{name}"
            );
        }
        assert!(check(&format!("file://{}/safe.txt", dir.display())).is_ok());
        assert!(check(&format!("file://{}/safe.txt%23fragment", dir.display())).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn decodes_percent_escapes() {
        assert_eq!(percent_decode("/a%20b").as_deref(), Some("/a b"));
        assert_eq!(percent_decode("/bad%2"), None);
    }
}
