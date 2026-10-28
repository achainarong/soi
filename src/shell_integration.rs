//! Shell integration for bash and zsh: soi starts them with a small script
//! that loads the user's own configuration, then marks prompts and commands
//! (OSC 133), as fish does on its own. That's what failed-command marks,
//! copying the last output and long-command notices need.
//!
//! As in kitty and Ghostty: zsh gets a `ZDOTDIR` whose `.zshenv` puts the
//! user's back and loads it; bash gets an `--rcfile` (only when started
//! plainly: a login shell, `bash -l`, reads its profile instead and is left
//! alone).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const BASH: &str = r#"# soi shell integration for bash; loads your own configuration first.
[ -f /etc/bash.bashrc ] && . /etc/bash.bashrc
[ -f ~/.bashrc ] && . ~/.bashrc

# Before each prompt: the last command's exit status, then where the
# prompt starts. Commands start where PS0 is printed.
__soi_prompt() {
    local code=$?
    __soi_at_prompt=0
    printf '\e]133;D;%s\a\e]133;A\a' "$code"
    return $code
}
if [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]]; then
    PROMPT_COMMAND=(__soi_prompt "${PROMPT_COMMAND[@]}")
else
    PROMPT_COMMAND="__soi_prompt${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
fi
if (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) )); then
    PS0="${PS0}\e]133;C\a"
elif [[ -z "$(trap -p DEBUG)" ]]; then
    # Apple's Bash 3.2 has no PS0. Do not replace a user's DEBUG trap.
    __soi_ready() { __soi_at_prompt=1; }
    __soi_preexec() {
        if [[ "${__soi_at_prompt:-0}" == 1 && "$BASH_COMMAND" != __soi_prompt && "$BASH_COMMAND" != __soi_ready ]]; then
            __soi_at_prompt=0
            printf '\e]133;C\a'
        fi
    }
    trap '__soi_preexec' DEBUG
    if [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]]; then
        PROMPT_COMMAND+=(__soi_ready)
    else
        PROMPT_COMMAND="${PROMPT_COMMAND};__soi_ready"
    fi
fi
"#;

const ZSHENV: &str = r#"# soi shell integration for zsh; puts your ZDOTDIR back and loads it.
if [[ -n "$SOI_ZDOTDIR" ]]; then
    ZDOTDIR="$SOI_ZDOTDIR"
else
    unset ZDOTDIR
fi
unset SOI_ZDOTDIR
[[ -f "${ZDOTDIR:-$HOME}/.zshenv" ]] && source "${ZDOTDIR:-$HOME}/.zshenv"

if [[ -o interactive ]]; then
    autoload -Uz add-zsh-hook
    # Before each prompt: the last command's exit status, then where the
    # prompt starts; before each command: that it starts, and what it is.
    __soi_precmd() {
        local code=$?
        printf '\e]133;D;%s\a\e]133;A\a' "$code"
    }
    __soi_preexec() {
        local line=${${1//$'\n'/ }[1,200]} url= c
        local i
        for (( i = 1; i <= ${#line}; i++ )); do
            c=${line[i]}
            case $c in
                [A-Za-z0-9._~/-]) url+=$c ;;
                *) url+=$(printf '%%%02X' "'$c") ;;
            esac
        done
        printf '\e]133;C;cmdline_url=%s\a' "$url"
    }
    add-zsh-hook precmd __soi_precmd
    add-zsh-hook preexec __soi_preexec
fi
"#;

/// The command and environment to start `argv` with, integration added
/// when it's bash or zsh. Anything else, or if the scripts can't be
/// written, starts as it is.
pub fn wrap(argv: Vec<String>, env: &mut Vec<(String, String)>) -> Vec<String> {
    let Some(dir) = scripts_dir() else {
        return argv;
    };
    match wrap_in(&dir, argv.clone(), env) {
        Ok(argv) => argv,
        Err(err) => {
            crate::log::write(&format!("shell integration unavailable: {err}"));
            argv
        }
    }
}

fn scripts_dir() -> Option<PathBuf> {
    Some(
        crate::dirs::data_home()
            .join("soi")
            .join("shell-integration"),
    )
}

fn wrap_in(
    dir: &Path,
    mut argv: Vec<String>,
    env: &mut Vec<(String, String)>,
) -> io::Result<Vec<String>> {
    let program = argv
        .first()
        .and_then(|p| Path::new(p).file_name())
        .and_then(|p| p.to_str())
        .unwrap_or_default();
    match program {
        "bash" if argv.len() == 1 => {
            let rcfile = dir.join("bash").join("soi.bash");
            write_if_changed(&rcfile, BASH)?;
            argv.extend(["--rcfile".into(), rcfile.to_string_lossy().into_owned()]);
        }
        "zsh" => {
            let zdotdir = dir.join("zsh");
            write_if_changed(&zdotdir.join(".zshenv"), ZSHENV)?;
            if let Ok(own) = std::env::var("ZDOTDIR") {
                env.push(("SOI_ZDOTDIR".into(), own));
            }
            env.push(("ZDOTDIR".into(), zdotdir.to_string_lossy().into_owned()));
        }
        _ => {}
    }
    Ok(argv)
}

fn write_if_changed(path: &Path, content: &str) -> io::Result<()> {
    if fs::read_to_string(path).is_ok_and(|old| old == content) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrapped(argv: &[&str]) -> (Vec<String>, Vec<(String, String)>) {
        let dir = std::env::temp_dir().join(format!("soi-shell-{}", uuid::Uuid::new_v4()));
        let mut env = Vec::new();
        let argv = wrap_in(&dir, argv.iter().map(|s| s.to_string()).collect(), &mut env).unwrap();
        fs::remove_dir_all(&dir).ok();
        (argv, env)
    }

    #[test]
    fn adds_integration_to_bash_and_zsh_only() {
        let (argv, env) = wrapped(&["/bin/bash"]);
        assert_eq!(argv[1], "--rcfile");
        assert!(argv[2].ends_with("soi.bash") && env.is_empty());

        let (argv, _) = wrapped(&["bash", "-l"]);
        assert_eq!(argv, ["bash", "-l"], "a login shell reads its profile");

        let (argv, env) = wrapped(&["/usr/bin/zsh", "-l"]);
        assert_eq!(argv, ["/usr/bin/zsh", "-l"]);
        assert!(
            env.iter()
                .any(|(k, v)| k == "ZDOTDIR" && v.ends_with("zsh"))
        );

        let (argv, env) = wrapped(&["fish"]);
        assert_eq!(argv, ["fish"]);
        assert!(env.is_empty(), "fish marks commands on its own");
    }

    #[test]
    fn bash_keeps_the_users_debug_trap() {
        let dir = std::env::temp_dir().join(format!("soi-bash-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join(".bashrc"), "trap ':' DEBUG\n").unwrap();
        let rcfile = dir.join("soi.bash");
        fs::write(&rcfile, BASH).unwrap();
        let output = std::process::Command::new("/bin/bash")
            .args([
                "--noprofile",
                "--norc",
                "-c",
                ". \"$1\"; trap -p DEBUG",
                "soi-test",
            ])
            .arg(&rcfile)
            .env("HOME", &dir)
            .env_remove("BASH_ENV")
            .output()
            .unwrap();
        fs::remove_dir_all(dir).unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("trap -- ':' DEBUG"));
    }
}
