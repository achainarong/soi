// libtest's worker threads cannot initialize AppKit/GTK on macOS.
fn main() -> std::process::ExitCode {
    soi::run_gui_tests()
}
