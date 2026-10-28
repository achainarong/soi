//! The terminal's widget: a plain widget that hands GTK render nodes
//! (`gtk::Snapshot`) instead of painting pixels with Cairo, so GTK draws
//! the text and backgrounds on the GPU and nothing is uploaded per frame.
//! It's also where screen readers get the terminal's text (see
//! `set_text_source`).

use std::cell::{Cell, RefCell};

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, graphene};

type DrawFn = Box<dyn Fn(&TermArea, &gtk::Snapshot)>;
type ResizeFn = Box<dyn Fn(i32, i32)>;

/// What screen readers read: the visible text and where the cursor is in
/// it (a character offset).
pub type TextSource = Box<dyn Fn() -> (String, u32)>;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct TermArea {
        pub draw: RefCell<Option<DrawFn>>,
        pub resize: RefCell<Option<ResizeFn>>,
        pub size: Cell<(i32, i32)>,
        pub text: RefCell<Option<TextSource>>,
        /// The text screen readers were last told about.
        pub told: RefCell<Vec<char>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TermArea {
        const NAME: &'static str = "SoiTermArea";
        type Type = super::TermArea;
        type ParentType = gtk::Widget;
        type Interfaces = (gtk::AccessibleText,);

        fn class_init(class: &mut Self::Class) {
            class.set_accessible_role(gtk::AccessibleRole::Terminal);
        }
    }

    impl ObjectImpl for TermArea {}

    impl WidgetImpl for TermArea {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if let Some(draw) = self.draw.borrow().as_ref() {
                draw(&self.obj(), snapshot);
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            if self.size.replace((width, height)) != (width, height)
                && let Some(resize) = self.resize.borrow().as_ref()
            {
                resize(width, height);
            }
        }
    }

    impl TermArea {
        pub(super) fn text(&self) -> (String, u32) {
            self.text.borrow().as_ref().map(|f| f()).unwrap_or_default()
        }
    }

    impl AccessibleTextImpl for TermArea {
        fn contents(&self, start: u32, end: u32) -> Option<glib::Bytes> {
            let (text, _) = self.text();
            Some(glib::Bytes::from_owned(
                slice_chars(&text, start, end).into_bytes(),
            ))
        }

        fn contents_at(
            &self,
            offset: u32,
            granularity: gtk::AccessibleTextGranularity,
        ) -> Option<(u32, u32, glib::Bytes)> {
            let (text, _) = self.text();
            let (start, end) = around(&text, offset, granularity);
            let bytes = glib::Bytes::from_owned(slice_chars(&text, start, end).into_bytes());
            Some((start, end, bytes))
        }

        fn caret_position(&self) -> u32 {
            self.text().1
        }

        fn selection(&self) -> Vec<gtk::AccessibleTextRange> {
            Vec::new()
        }

        fn attributes(
            &self,
            _offset: u32,
        ) -> Vec<(gtk::AccessibleTextRange, glib::GString, glib::GString)> {
            Vec::new()
        }

        fn default_attributes(&self) -> Vec<(glib::GString, glib::GString)> {
            Vec::new()
        }
    }
}

glib::wrapper! {
    pub struct TermArea(ObjectSubclass<imp::TermArea>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::AccessibleText, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for TermArea {
    fn default() -> Self {
        glib::Object::builder()
            .property("hexpand", true)
            .property("vexpand", true)
            .property("focusable", true)
            .property("can-focus", true)
            .property("overflow", gtk::Overflow::Hidden)
            .build()
    }
}

impl TermArea {
    pub fn set_draw_func(&self, f: impl Fn(&TermArea, &gtk::Snapshot) + 'static) {
        self.imp().draw.replace(Some(Box::new(f)));
    }

    /// Runs `f` with the new width and height whenever the size changes.
    pub fn connect_resize(&self, f: impl Fn(i32, i32) + 'static) {
        self.imp().resize.replace(Some(Box::new(f)));
    }

    pub fn set_text_source(&self, f: impl Fn() -> (String, u32) + 'static) {
        self.imp().text.replace(Some(Box::new(f)));
    }

    /// Tells screen readers what changed in the text since last time
    /// (just that part, so they don't read the whole screen again), and
    /// where the cursor is.
    pub fn text_changed(&self) {
        let imp = self.imp();
        let new: Vec<char> = imp.text().0.chars().collect();
        let old = imp.told.replace(new.clone());
        if let Some((start, removed, inserted)) = changed_range(&old, &new) {
            if removed > start {
                self.update_contents(gtk::AccessibleTextContentChange::Remove, start, removed);
            }
            if inserted > start {
                self.update_contents(gtk::AccessibleTextContentChange::Insert, start, inserted);
            }
        }
        self.update_caret_position();
    }

    pub fn bounds(&self) -> graphene::Rect {
        graphene::Rect::new(0.0, 0.0, self.width() as f32, self.height() as f32)
    }
}

/// Where `new` differs from `old`: the first changed character, and the
/// end of the changed part in `old` and in `new`. `None` when the same.
fn changed_range(old: &[char], new: &[char]) -> Option<(u32, u32, u32)> {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    if prefix == old.len() && prefix == new.len() {
        return None;
    }
    let max_suffix = old.len().min(new.len()) - prefix;
    let suffix = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(max_suffix)
        .take_while(|(a, b)| a == b)
        .count();
    Some((
        prefix as u32,
        (old.len() - suffix) as u32,
        (new.len() - suffix) as u32,
    ))
}

/// Characters `start..end` of `text`.
fn slice_chars(text: &str, start: u32, end: u32) -> String {
    text.chars()
        .skip(start as usize)
        .take(end.saturating_sub(start) as usize)
        .collect()
}

/// The character, word, sentence-ish line or paragraph around `offset`, as
/// a range of characters.
fn around(text: &str, offset: u32, granularity: gtk::AccessibleTextGranularity) -> (u32, u32) {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let at = (offset as usize).min(len);
    let (start, end) = match granularity {
        gtk::AccessibleTextGranularity::Character => (at, (at + 1).min(len)),
        gtk::AccessibleTextGranularity::Word => {
            let word = |c: &char| !c.is_whitespace();
            let start = chars[..at]
                .iter()
                .rposition(|c| !word(c))
                .map_or(0, |i| i + 1);
            let end = chars[at..]
                .iter()
                .position(|c| !word(c))
                .map_or(len, |i| at + i);
            (start, end)
        }
        // Lines are what a terminal has; sentences and paragraphs are lines.
        _ => {
            let start = chars[..at]
                .iter()
                .rposition(|&c| c == '\n')
                .map_or(0, |i| i + 1);
            let end = chars[at..]
                .iter()
                .position(|&c| c == '\n')
                .map_or(len, |i| at + i + 1);
            (start, end)
        }
    };
    (start as u32, end as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk::AccessibleTextGranularity as G;

    #[test]
    fn reports_only_what_changed() {
        let chars = |s: &str| s.chars().collect::<Vec<_>>();
        let changed = |a: &str, b: &str| changed_range(&chars(a), &chars(b));
        assert_eq!(changed("$ ls\n", "$ ls\n"), None);
        // Typed a letter: one inserted at 4.
        assert_eq!(changed("$ ls\n", "$ lsx\n"), Some((4, 4, 5)));
        // A line replaced in the middle.
        assert_eq!(changed("a\nbb\nc\n", "a\nXY\nc\n"), Some((2, 4, 4)));
        assert_eq!(changed("", "hi"), Some((0, 0, 2)));
        assert_eq!(changed("aaa", "aa"), Some((2, 3, 2)));
    }

    #[test]
    fn finds_characters_words_and_lines() {
        let text = "ls -la\nfoo bar\n";
        assert_eq!(around(text, 1, G::Character), (1, 2));
        assert_eq!(around(text, 1, G::Word), (0, 2));
        assert_eq!(around(text, 9, G::Word), (7, 10));
        assert_eq!(around(text, 9, G::Line), (7, 15));
        assert_eq!(around(text, 0, G::Line), (0, 7));
        assert_eq!(around(text, 99, G::Line), (15, 15));
        assert_eq!(slice_chars("héllo", 1, 3), "él");
    }
}
