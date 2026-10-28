//! A main-thread harness: each check starts GTK in its own isolated process.

use std::process::ExitCode;

pub(crate) fn run() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (exact, filter) = match args.as_slice() {
        [] => (false, ""),
        [flag] if flag == "--list" => {
            for (name, _) in checks() {
                println!("{name}");
            }
            return ExitCode::SUCCESS;
        }
        [flag] if flag == "--help" || flag == "-h" => {
            println!("Usage: gui [--list | FILTER | --exact NAME]");
            return ExitCode::SUCCESS;
        }
        [filter] if !filter.starts_with('-') => (false, filter.as_str()),
        [flag, filter] if flag == "--exact" => (true, filter.as_str()),
        _ => {
            eprintln!("Usage: gui [--list | FILTER | --exact NAME]");
            return ExitCode::FAILURE;
        }
    };
    let selected: Vec<_> = checks()
        .filter(|(name, _)| {
            if exact {
                *name == filter
            } else {
                name.contains(filter)
            }
        })
        .collect();
    if selected.is_empty() {
        eprintln!("No GUI checks match {filter:?}");
        return ExitCode::FAILURE;
    }
    let mut failed = 0;
    for (name, check) in &selected {
        let result = std::panic::catch_unwind(check);
        println!(
            "{name} ... {}",
            if result.is_ok() { "ok" } else { "FAILED" }
        );
        failed += usize::from(result.is_err());
    }
    println!(
        "GUI checks: {} passed; {failed} failed",
        selected.len() - failed
    );
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn checks() -> impl Iterator<Item = (&'static str, fn())> {
    super::window_tests::CHECKS
        .iter()
        .copied()
        .chain([("gtk_checks", super::css_tests::gtk_checks as fn())])
}
