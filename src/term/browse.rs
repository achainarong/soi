//! Browse mode: moving through the terminal's output with vim keys,
//! searching it, selecting and copying, and jumping between commands.
//! While browsing, keys go here instead of to the program.

use super::screen::Term;

/// A position: absolute line and column.
pub type Pos = (i64, usize);

/// The largest count prefix (`99999j`), so count arithmetic can't overflow.
const MAX_COUNT: usize = 99_999;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Ctrl(char),
    Escape,
    Enter,
    Backspace,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
}

/// What the widget should do after a key.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Exit,
    /// Copy this text, then leave browse mode.
    Copy(String),
    OpenLink(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visual {
    Chars,
    Lines,
}

#[derive(Clone, Debug)]
pub struct Search {
    pub pattern: String,
    pub forward: bool,
}

#[derive(Debug)]
pub struct Browse {
    pub cursor: Pos,
    /// Visual selection: its kind and where it started.
    pub visual: Option<(Visual, Pos)>,
    pub search: Option<Search>,
    /// A search being typed: direction and text so far.
    pub typing: Option<(bool, String)>,
    /// Shown in the status line until the next key.
    pub message: Option<String>,
    /// First key of a two-key command (`gg`, `yy`, `[[`, `]]`).
    pending: Option<char>,
    count: Option<usize>,
    /// Column `j`/`k` try to keep, like vim.
    want_col: usize,
}

/// Characters of the same class form a word (vim's `w`/`b`/`e`).
fn class(c: char) -> u8 {
    if c.is_whitespace() {
        0
    } else if c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

/// Smart case: case-insensitive unless the pattern has capitals.
fn matcher(pattern: &str) -> impl Fn(&[char], usize) -> bool + '_ {
    let ignore_case = !pattern.chars().any(char::is_uppercase);
    let pattern: Vec<char> = pattern.chars().collect();
    move |chars: &[char], at: usize| {
        chars.len() >= at + pattern.len()
            && pattern.iter().zip(&chars[at..]).all(|(&p, &c)| {
                if ignore_case {
                    c.to_lowercase().eq(p.to_lowercase())
                } else {
                    c == p
                }
            })
    }
}

/// All matches of `pattern` in a line, as column ranges (end exclusive).
pub fn line_matches(term: &Term, abs: i64, pattern: &str) -> Vec<(usize, usize)> {
    let n = pattern.chars().count();
    let Some((chars, columns)) = term.line_chars(abs) else {
        return Vec::new();
    };
    if n == 0 || chars.len() < n {
        return Vec::new();
    }
    let is_match = matcher(pattern);
    (0..=chars.len() - n)
        .filter(|&i| is_match(&chars, i))
        .map(|i| {
            let last = columns[i + n - 1];
            let end = last + if term.is_wide(abs, last) { 2 } else { 1 };
            (columns[i], end)
        })
        .collect()
}

impl Browse {
    /// Starts at the terminal's cursor when the view is at the bottom,
    /// otherwise at the top of the view.
    pub fn new(term: &Term, view_top: i64, at_bottom: bool) -> Self {
        let cursor = match (at_bottom, term.cursor()) {
            (true, Some((row, col))) => (term.scrolled_off() + row as i64, col),
            _ => (view_top, 0),
        };
        Self {
            cursor,
            visual: None,
            search: None,
            typing: None,
            message: None,
            pending: None,
            count: None,
            want_col: cursor.1,
        }
    }

    /// The selected range, inclusive, for highlighting and copying.
    pub fn selection(&self, term: &Term) -> Option<(Pos, Pos)> {
        let (kind, anchor) = self.visual?;
        let (start, end) = if anchor <= self.cursor {
            (anchor, self.cursor)
        } else {
            (self.cursor, anchor)
        };
        Some(match kind {
            Visual::Chars => (start, end),
            Visual::Lines => ((start.0, 0), (end.0, term.size().0 - 1)),
        })
    }

    pub fn status(&self, term: &Term) -> String {
        if let Some((forward, text)) = &self.typing {
            return format!("{}{text}", if *forward { '/' } else { '?' });
        }
        let mode = match self.visual {
            Some((Visual::Chars, _)) => "VISUAL",
            Some((Visual::Lines, _)) => "VISUAL LINE",
            None => "BROWSE",
        };
        let (first, last) = term.line_range();
        let position = format!("{}/{}", self.cursor.0 - first + 1, last - first + 1);
        match &self.message {
            Some(message) => format!("-- {mode} --  {message}  {position}"),
            None => format!("-- {mode} --  {position}   q: quit  /: search  v: select  y: copy"),
        }
    }

    pub fn handle(&mut self, term: &Term, key: Key, view_top: i64, view_rows: usize) -> Action {
        self.message = None;
        if self.typing.is_some() {
            return self.handle_typing(term, key);
        }
        let count = self.count.take();
        let n = count.unwrap_or(1);
        let pending = self.pending.take();
        let (first, last) = term.line_range();
        let cols = term.size().0;
        let (line, col) = self.cursor;
        let half = (view_rows / 2).max(1) as i64;
        let page = view_rows.max(1) as i64;

        match (pending, key) {
            (Some('g'), Key::Char('g')) => {
                let line = count.map_or(first, |n| first + n as i64 - 1);
                self.go_line(term, line);
            }
            (Some('y'), Key::Char('y')) => {
                let (from, to) = (line, (line + n as i64 - 1).min(last));
                return Action::Copy(term.text_between((from, 0), (to, cols - 1)));
            }
            (Some('['), Key::Char('[')) => match term.prompts().rev().find(|&p| p < line) {
                Some(prompt) => self.cursor = (prompt, 0),
                None => self.message = Some("no earlier command".into()),
            },
            (Some(']'), Key::Char(']')) => match term.prompts().find(|&p| p > line) {
                Some(prompt) => self.cursor = (prompt, 0),
                None => self.message = Some("no later command".into()),
            },
            (Some(_), _) => {} // unknown two-key command: drop it

            (None, Key::Char(c @ '1'..='9')) => {
                let digit = c.to_digit(10).unwrap_or(0) as usize;
                self.count = Some((count.unwrap_or(0) * 10 + digit).min(MAX_COUNT));
            }
            (None, Key::Char('0')) if count.is_some() => {
                self.count = count.map(|n| (n * 10).min(MAX_COUNT));
            }
            (None, Key::Char(c @ ('g' | 'y' | '[' | ']')))
                if !(c == 'y' && self.visual.is_some()) =>
            {
                self.pending = Some(c);
                self.count = count;
            }

            (None, Key::Char('q' | 'i')) => return Action::Exit,
            (None, Key::Escape) => {
                if self.visual.take().is_none() {
                    return Action::Exit;
                }
            }
            (None, Key::Char('v')) => self.toggle_visual(Visual::Chars),
            (None, Key::Char('V')) => self.toggle_visual(Visual::Lines),
            (None, Key::Char('y')) => {
                if let Some((start, end)) = self.selection(term) {
                    return Action::Copy(term.text_between(start, end));
                }
            }
            (None, Key::Char('o') | Key::Enter) => match term.link_at(line, col) {
                Some(link) => return Action::OpenLink(link.url),
                None => self.message = Some("no link here".into()),
            },

            (None, Key::Char('h') | Key::Left) => self.go_col(col.saturating_sub(n)),
            (None, Key::Char('l') | Key::Right) => self.go_col((col + n).min(cols - 1)),
            (None, Key::Char('j') | Key::Down) => self.go_line(term, line + n as i64),
            (None, Key::Char('k') | Key::Up) => self.go_line(term, line - n as i64),
            (None, Key::Char('0') | Key::Home) => self.go_col(0),
            (None, Key::Char('^')) => {
                let first_text = term
                    .line_chars(line)
                    .and_then(|(chars, columns)| {
                        chars
                            .iter()
                            .position(|c| !c.is_whitespace())
                            .map(|i| columns[i])
                    })
                    .unwrap_or(0);
                self.go_col(first_text);
            }
            (None, Key::Char('$') | Key::End) => {
                let end = term
                    .line_chars(line)
                    .and_then(|(chars, columns)| {
                        chars.iter().rposition(|c| *c != ' ').map(|i| columns[i])
                    })
                    .unwrap_or(0);
                self.go_col(end);
            }
            (None, Key::Char('w')) => (0..n).for_each(|_| self.word(term, true, false)),
            (None, Key::Char('b')) => (0..n).for_each(|_| self.word(term, false, false)),
            (None, Key::Char('e')) => (0..n).for_each(|_| self.word(term, true, true)),
            (None, Key::Char('G')) => {
                let line = count.map_or(last, |n| first + n as i64 - 1);
                self.go_line(term, line);
            }
            (None, Key::Char('H')) => self.go_line(term, view_top),
            (None, Key::Char('M')) => self.go_line(term, view_top + view_rows as i64 / 2),
            (None, Key::Char('L')) => self.go_line(term, view_top + view_rows as i64 - 1),
            (None, Key::Ctrl('d')) => self.go_line(term, line + half * n as i64),
            (None, Key::Ctrl('u')) => self.go_line(term, line - half * n as i64),
            (None, Key::Ctrl('f') | Key::PageDown) => self.go_line(term, line + page * n as i64),
            (None, Key::Ctrl('b') | Key::PageUp) => self.go_line(term, line - page * n as i64),

            (None, Key::Char('/')) => self.typing = Some((true, String::new())),
            (None, Key::Char('?')) => self.typing = Some((false, String::new())),
            (None, Key::Char('n')) => self.repeat_search(term, false, n),
            (None, Key::Char('N')) => self.repeat_search(term, true, n),
            (None, Key::Char('*')) => {
                if let Some(word) = self.word_under_cursor(term) {
                    self.search = Some(Search {
                        pattern: word,
                        forward: true,
                    });
                    self.repeat_search(term, false, n);
                }
            }
            _ => {}
        }
        Action::None
    }

    fn handle_typing(&mut self, term: &Term, key: Key) -> Action {
        let Some((forward, text)) = &mut self.typing else {
            return Action::None;
        };
        match key {
            Key::Char(c) => text.push(c),
            Key::Backspace => {
                if text.pop().is_none() {
                    self.typing = None;
                }
            }
            Key::Escape => self.typing = None,
            Key::Enter => {
                let (forward, pattern) = (*forward, std::mem::take(text));
                self.typing = None;
                if !pattern.is_empty() {
                    self.search = Some(Search { pattern, forward });
                }
                self.repeat_search(term, false, 1);
            }
            _ => {}
        }
        Action::None
    }

    fn toggle_visual(&mut self, kind: Visual) {
        self.visual = match self.visual {
            Some((current, _)) if current == kind => None,
            Some((_, anchor)) => Some((kind, anchor)),
            None => Some((kind, self.cursor)),
        };
    }

    fn go_col(&mut self, col: usize) {
        self.cursor.1 = col;
        self.want_col = col;
    }

    fn go_line(&mut self, term: &Term, line: i64) {
        let (first, last) = term.line_range();
        let cols = term.size().0;
        self.cursor = (line.clamp(first, last), self.want_col.min(cols - 1));
    }

    /// Moves to the next/previous word start, or the next word end.
    fn word(&mut self, term: &Term, forward: bool, to_end: bool) {
        let (first, last) = term.line_range();
        // Walk through characters across lines; line ends count as spaces.
        let mut chars: Vec<(char, Pos)> = Vec::new();
        let lines = if forward {
            self.cursor.0..=(self.cursor.0 + 200).min(last)
        } else {
            (self.cursor.0 - 200).max(first)..=self.cursor.0
        };
        for abs in lines {
            if let Some((line, columns)) = term.line_chars(abs) {
                chars.extend(line.into_iter().zip(columns.into_iter().map(|c| (abs, c))));
                chars.push((' ', (abs, usize::MAX)));
            }
        }
        let Some(here) = chars.iter().position(|(_, pos)| *pos >= self.cursor) else {
            return;
        };
        let target = if forward {
            let mut i = here;
            if to_end {
                // Skip to the next non-space, then to the end of its word.
                i += 1;
                while i < chars.len() && class(chars[i].0) == 0 {
                    i += 1;
                }
                while i + 1 < chars.len() && class(chars[i + 1].0) == class(chars[i].0) {
                    i += 1;
                }
            } else {
                let start_class = class(chars[i].0);
                while i < chars.len() && class(chars[i].0) == start_class && start_class != 0 {
                    i += 1;
                }
                while i < chars.len() && class(chars[i].0) == 0 {
                    i += 1;
                }
            }
            i
        } else {
            let mut i = here;
            while i > 0 && class(chars[i - 1].0) == 0 {
                i -= 1;
            }
            i = i.saturating_sub(1);
            while i > 0 && class(chars[i - 1].0) == class(chars[i].0) && class(chars[i].0) != 0 {
                i -= 1;
            }
            i
        };
        if let Some((_, pos)) = chars.get(target)
            && pos.1 != usize::MAX
        {
            self.cursor = *pos;
            self.want_col = pos.1;
        }
    }

    fn word_under_cursor(&self, term: &Term) -> Option<String> {
        let (chars, columns) = term.line_chars(self.cursor.0)?;
        let at = columns.iter().rposition(|&c| c <= self.cursor.1)?;
        if class(chars[at]) != 1 {
            return None;
        }
        let start = (0..=at)
            .rev()
            .take_while(|&i| class(chars[i]) == 1)
            .last()?;
        let end = (at..chars.len())
            .take_while(|&i| class(chars[i]) == 1)
            .last()?
            + 1;
        Some(chars[start..end].iter().collect())
    }

    /// Jumps to the next match of the current search (`reverse` for `N`),
    /// wrapping around at the ends like vim.
    fn repeat_search(&mut self, term: &Term, reverse: bool, times: usize) {
        let Some(search) = self.search.clone() else {
            self.message = Some("no previous search".into());
            return;
        };
        let forward = search.forward != reverse;
        for _ in 0..times {
            match self.find(term, &search.pattern, forward) {
                Some((pos, wrapped)) => {
                    self.cursor = pos;
                    self.want_col = pos.1;
                    if wrapped {
                        self.message = Some(if forward {
                            "search hit BOTTOM, continuing at TOP".into()
                        } else {
                            "search hit TOP, continuing at BOTTOM".into()
                        });
                    }
                }
                None => {
                    self.message = Some(format!("pattern not found: {}", search.pattern));
                    return;
                }
            }
        }
        let (first, _) = term.line_range();
        let total: usize = (first..=term.line_range().1)
            .map(|abs| line_matches(term, abs, &search.pattern).len())
            .sum();
        if self.message.is_none() {
            self.message = Some(format!("{total} matches"));
        }
    }

    /// The next match after (or before) the cursor, and whether the search
    /// wrapped around.
    fn find(&self, term: &Term, pattern: &str, forward: bool) -> Option<(Pos, bool)> {
        let (first, last) = term.line_range();
        let span = (last - first + 1) as usize;
        let here = self.cursor;
        for step in 0..=span {
            let offset = step as i64;
            let raw = if forward {
                here.0 + offset
            } else {
                here.0 - offset
            };
            let wrapped = raw > last || raw < first;
            let abs = first + (raw - first).rem_euclid(span as i64);
            let matches = line_matches(term, abs, pattern);
            let pick = if forward {
                matches
                    .iter()
                    .find(|(start, _)| step > 0 || *start > here.1)
                    .or_else(|| (step == span).then(|| matches.first()).flatten())
            } else {
                matches
                    .iter()
                    .rev()
                    .find(|(start, _)| step > 0 || *start < here.1)
            };
            if let Some((start, _)) = pick {
                return Some(((abs, *start), wrapped));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(text: &str) -> Term {
        let mut term = Term::new(20, 5, 100);
        let mut parser = vte::Parser::new();
        parser.advance(&mut term, text.as_bytes());
        term
    }

    fn keys(browse: &mut Browse, term: &Term, keys: &str) -> Action {
        let mut action = Action::None;
        for c in keys.chars() {
            let key = match c {
                '\n' => Key::Enter,
                '\x1b' => Key::Escape,
                c => Key::Char(c),
            };
            action = browse.handle(term, key, 0, 5);
        }
        action
    }

    #[test]
    fn moves_like_vim() {
        let term = term("one two three\r\nfour");
        let mut browse = Browse::new(&term, 0, false);
        keys(&mut browse, &term, "w");
        assert_eq!(browse.cursor, (0, 4));
        keys(&mut browse, &term, "e");
        assert_eq!(browse.cursor, (0, 6));
        keys(&mut browse, &term, "$");
        assert_eq!(browse.cursor, (0, 12));
        keys(&mut browse, &term, "j0");
        assert_eq!(browse.cursor, (1, 0));
        keys(&mut browse, &term, "b");
        assert_eq!(browse.cursor, (0, 8));
        keys(&mut browse, &term, "G");
        assert_eq!(browse.cursor.0, 4);
        keys(&mut browse, &term, "gg");
        assert_eq!(browse.cursor.0, 0);
        keys(&mut browse, &term, "2j");
        assert_eq!(browse.cursor.0, 2);
    }

    #[test]
    fn huge_count_moves_to_the_edge_without_overflowing() {
        let term = term("one\r\ntwo\r\nthree");
        let mut browse = Browse::new(&term, 0, false);
        keys(&mut browse, &term, &"9".repeat(30));
        keys(&mut browse, &term, "j");
        assert_eq!(browse.cursor.0, 4);
        keys(&mut browse, &term, &format!("{}0k", "9".repeat(30)));
        assert_eq!(browse.cursor.0, 0);
    }

    #[test]
    fn searches_with_smart_case_and_wraps() {
        let term = term("Alpha beta\r\nalpha\r\nALPHA");
        let mut browse = Browse::new(&term, 0, false);
        keys(&mut browse, &term, "/alpha\n");
        assert_eq!(browse.cursor, (1, 0));
        keys(&mut browse, &term, "n");
        assert_eq!(browse.cursor, (2, 0));
        keys(&mut browse, &term, "n");
        assert_eq!(browse.cursor, (0, 0), "wraps around");
        assert!(browse.message.as_deref().unwrap().contains("BOTTOM"));
        keys(&mut browse, &term, "/ALPHA\n");
        assert_eq!(browse.cursor, (2, 0), "capitals make it case-sensitive");
        keys(&mut browse, &term, "/nothing\n");
        assert!(browse.message.as_deref().unwrap().contains("not found"));
    }

    #[test]
    fn selects_and_copies() {
        let term = term("hello world\r\nsecond line");
        let mut browse = Browse::new(&term, 0, false);
        let action = keys(&mut browse, &term, "wve");
        assert_eq!(action, Action::None);
        assert_eq!(keys(&mut browse, &term, "y"), Action::Copy("world".into()));
        let mut browse = Browse::new(&term, 0, false);
        assert_eq!(
            keys(&mut browse, &term, "Vjy"),
            Action::Copy("hello world\nsecond line".into())
        );
        let mut browse = Browse::new(&term, 0, false);
        assert_eq!(
            keys(&mut browse, &term, "jyy"),
            Action::Copy("second line".into())
        );
    }

    #[test]
    fn jumps_between_prompts() {
        let term = term("\x1b]133;A\x07$ ls\r\nfile\r\n\x1b]133;A\x07$ pwd\r\n/home\r\n");
        let mut browse = Browse::new(&term, 4, false);
        browse.cursor = (4, 0);
        keys(&mut browse, &term, "[[");
        assert_eq!(browse.cursor, (2, 0));
        keys(&mut browse, &term, "[[");
        assert_eq!(browse.cursor, (0, 0));
        keys(&mut browse, &term, "]]");
        assert_eq!(browse.cursor, (2, 0));
    }

    #[test]
    fn leaves_visual_before_browse_mode() {
        let term = term("text");
        let mut browse = Browse::new(&term, 0, false);
        keys(&mut browse, &term, "v");
        assert_eq!(keys(&mut browse, &term, "\x1b"), Action::None);
        assert_eq!(keys(&mut browse, &term, "\x1b"), Action::Exit);
    }
}
