//! Needs a display: `make test-gui`.

use super::*;

pub(super) fn gtk_checks() {
    super::window_tests::isolated("gtk_checks", || {
        gtk::init().unwrap();
        theme_css_parses_cleanly();
        default_shortcuts_are_valid();
    });
}

fn theme_css_parses_cleanly() {
    let mut themes = vec![TerminalTheme::detect()];
    for id in ["builtin:Nord", "builtin:Catppuccin Latte"] {
        themes.push(crate::theme::named_scheme(id).unwrap());
    }
    for theme in themes {
        let errors = Rc::new(RefCell::new(Vec::new()));
        let provider = gtk::CssProvider::new();
        let sink = errors.clone();
        provider.connect_parsing_error(move |_, _, error| {
            sink.borrow_mut().push(error.to_string());
        });
        provider.load_from_string(&theme_css(&theme, 0.75, 1.0));
        provider.load_from_string(CSS);
        assert!(errors.borrow().is_empty(), "{:?}", errors.borrow());
    }
    println!("{}", theme_css(&TerminalTheme::detect(), 0.75, 1.0));
}

fn default_shortcuts_are_valid() {
    for binding in BINDINGS.iter().filter(|b| !b.default_accel().is_empty()) {
        let (key, mods) = parse_accel(binding.default_accel())
            .unwrap_or_else(|| panic!("{} doesn't parse", binding.id));
        // (Not gtk::accelerator_valid: it rejects Tab, which works fine.)
        assert!(
            key.name().is_some() && !mods.is_empty() || binding.id == "rename-workspace",
            "{}",
            binding.id
        );
    }
    assert!(same_accel("<Control><Shift>T", "<Shift><Control>t"));
    assert!(!same_accel("<Control>t", "<Shift><Control>t"));
    assert_eq!(accel_label(""), "Off");
    assert!(parse_accel("").is_none() && !same_accel("", ""));
}
