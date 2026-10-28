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
    for binding in BINDINGS {
        for accel in keybindings::accels(binding.default_accel()) {
            let (key, mods) =
                parse_accel(accel).unwrap_or_else(|| panic!("{} doesn't parse", binding.id));
            // (Not gtk::accelerator_valid: it rejects Tab, which works fine.)
            assert!(
                key.name().is_some() && !mods.is_empty() || binding.id == "rename-workspace",
                "{}",
                binding.id
            );
        }
    }
    assert!(same_accel("<Control><Shift>T", "<Shift><Control>t"));
    assert!(!same_accel("<Control>t", "<Shift><Control>t"));
    // Several keys: any one in common is the same.
    assert!(same_accel(
        "<Meta>Return|<Control><Alt>Escape",
        "<Control><Alt>Escape"
    ));
    assert!(same_accel("<Alt>x", "<Meta>Return|<Alt>X"));
    assert!(!same_accel(
        "<Meta>Return|<Control><Alt>Escape",
        "<Meta>Escape"
    ));
    assert_eq!(
        accel_label("<Control>t|<Alt>x"),
        format!("{} or {}", accel_label("<Control>t"), accel_label("<Alt>x"))
    );
    assert_eq!(accel_label(""), "Off");
    assert!(parse_accel("").is_none() && !same_accel("", ""));
}
