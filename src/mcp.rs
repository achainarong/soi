//! MCP servers to start with an agent for a task. Unlike skills and
//! commands, which the brief only names, a server must be loaded when the
//! agent starts: Claude Code gets them with `--mcp-config`, OpenCode in
//! `OPENCODE_CONFIG_CONTENT`; Pi has no MCP. Found in soi's own
//! `~/.config/soi/mcp.json` and in the agents' configurations, which are only
//! read, never changed. Servers are kept in Claude Code's form.
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

#[derive(Debug, Clone, PartialEq)]
pub struct Server {
    pub name: String,
    /// As in Claude Code's `mcpServers`: `command`, `args` and `env`, or
    /// `type` (`http`, `sse`) with `url` and `headers`.
    pub config: Value,
    /// The file it was found in.
    pub path: PathBuf,
}

impl Server {
    /// What it runs or where it connects, for a tooltip; never its
    /// environment or headers, which may hold secrets.
    pub fn description(&self) -> String {
        if let Some(url) = self.config.get("url").and_then(Value::as_str) {
            return url.to_string();
        }
        let command = self
            .config
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("");
        let args = self
            .config
            .get("args")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str);
        std::iter::once(command)
            .chain(args)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// soi's own servers, for Settings ▸ Collections to edit.
pub fn own_path() -> PathBuf {
    crate::dirs::config_home().join("soi/mcp.json")
}

/// What to start editing soi's own servers from.
pub const OWN_TEMPLATE: &str = r#"{
  "mcpServers": {
  }
}
"#;

/// Checks `text` is a `mcp.json` before it is saved; the error says where.
pub fn check_own(text: &str) -> Result<(), String> {
    let json: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let servers = json
        .get("mcpServers")
        .ok_or("It needs an \"mcpServers\" object.")?
        .as_object()
        .ok_or("\"mcpServers\" must be an object.")?;
    for (name, config) in servers {
        let has = |key| config.get(key).and_then(Value::as_str).is_some();
        if !has("command") && !has("url") {
            return Err(format!("{name} needs a \"command\" or a \"url\"."));
        }
    }
    Ok(())
}

pub fn save_own(text: &str) -> io::Result<()> {
    check_own(text).map_err(io::Error::other)?;
    // It may hold tokens.
    crate::dirs::write_atomic(&own_path(), text.as_bytes(), 0o600)
}

/// The servers available for work in `repository`, by name; where two share
/// a name, the first found wins: soi's own, the repository's, then Claude
/// Code's and OpenCode's for the user.
pub fn discover(repository: &Path) -> Vec<Server> {
    discover_in(&crate::dirs::home_dir(), &own_path(), repository)
}

pub fn discover_in(home: &Path, own: &Path, repository: &Path) -> Vec<Server> {
    let claude = home.join(".claude.json");
    let claude_json = read(&claude);
    let project = claude_json
        .as_ref()
        .and_then(|json| json.get("projects")?.get(repository.to_str()?))
        .and_then(|project| project.get("mcpServers"));
    let opencode = home.join(".config/opencode/opencode.json");
    let repository_opencode = repository.join("opencode.json");
    let found = [
        (
            own.to_path_buf(),
            read(own).and_then(|j| j.get("mcpServers").cloned()),
            false,
        ),
        (
            repository.join(".mcp.json"),
            read(&repository.join(".mcp.json")).and_then(|j| j.get("mcpServers").cloned()),
            false,
        ),
        (claude.clone(), project.cloned(), false),
        (
            repository_opencode.clone(),
            read(&repository_opencode).and_then(|j| j.get("mcp").cloned()),
            true,
        ),
        (
            claude,
            claude_json
                .as_ref()
                .and_then(|j| j.get("mcpServers").cloned()),
            false,
        ),
        (
            opencode.clone(),
            read(&opencode).and_then(|j| j.get("mcp").cloned()),
            true,
        ),
    ];
    let mut servers: Vec<Server> = Vec::new();
    for (path, list, opencode) in found {
        let Some(Value::Object(list)) = list else {
            continue;
        };
        for (name, config) in list {
            let config = if opencode {
                from_opencode(&config)
            } else {
                Some(config)
            };
            if let Some(config) = config
                && !servers.iter().any(|s| s.name == name)
            {
                servers.push(Server {
                    name,
                    config,
                    path: path.clone(),
                });
            }
        }
    }
    servers.sort_by_key(|server| server.name.to_lowercase());
    servers
}

fn read(path: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

/// The servers named in `names` that are found for `repository`; those no
/// longer found are left out.
pub fn chosen(names: &[String], repository: &Path) -> Vec<Server> {
    discover(repository)
        .into_iter()
        .filter(|server| names.contains(&server.name))
        .collect()
}

/// An OpenCode `mcp` entry in Claude Code's form.
fn from_opencode(config: &Value) -> Option<Value> {
    match config.get("type").and_then(Value::as_str)? {
        "local" => {
            let command = config.get("command")?.as_array()?;
            let mut out = json!({
                "command": command.first()?.as_str()?,
                "args": command[1..].to_vec(),
            });
            if let Some(env) = config.get("environment") {
                out["env"] = env.clone();
            }
            Some(out)
        }
        "remote" => {
            let mut out = json!({ "type": "http", "url": config.get("url")?.as_str()? });
            if let Some(headers) = config.get("headers") {
                out["headers"] = headers.clone();
            }
            Some(out)
        }
        _ => None,
    }
}

/// A Claude Code server in OpenCode's form.
fn to_opencode(config: &Value) -> Value {
    if let Some(url) = config.get("url") {
        let mut out = json!({ "type": "remote", "url": url });
        if let Some(headers) = config.get("headers") {
            out["headers"] = headers.clone();
        }
        return out;
    }
    let mut command = vec![config.get("command").cloned().unwrap_or_default()];
    if let Some(Value::Array(args)) = config.get("args") {
        command.extend(args.iter().cloned());
    }
    let mut out = json!({ "type": "local", "command": command });
    if let Some(env) = config.get("env") {
        out["environment"] = env.clone();
    }
    out
}

/// Has `program` start with `servers`: Claude Code reads them from `file`,
/// written here (it may hold tokens, so only the user may read it; the
/// command line is visible to every process); OpenCode from its
/// environment, on top of what soi already sets there.
pub fn apply(
    program: &str,
    servers: &[Server],
    file: &Path,
    argv: &mut Vec<String>,
    env: &mut Vec<(String, String)>,
) -> io::Result<()> {
    if servers.is_empty() {
        return Ok(());
    }
    match program {
        "claude" => {
            let list: Map<String, Value> = servers
                .iter()
                .map(|s| (s.name.clone(), s.config.clone()))
                .collect();
            let text = serde_json::to_string_pretty(&json!({ "mcpServers": list }))?;
            crate::dirs::write_atomic(file, text.as_bytes(), 0o600)?;
            argv.extend(["--mcp-config".into(), file.to_string_lossy().into_owned()]);
        }
        "opencode" => {
            const KEY: &str = "OPENCODE_CONFIG_CONTENT";
            let at = env.iter().position(|(key, _)| key == KEY);
            let mut config = at
                .and_then(|i| serde_json::from_str(&env[i].1).ok())
                .unwrap_or_else(|| json!({}));
            let list: Map<String, Value> = servers
                .iter()
                .map(|s| (s.name.clone(), to_opencode(&s.config)))
                .collect();
            config["mcp"] = Value::Object(list);
            let value = config.to_string();
            match at {
                Some(i) => env[i].1 = value,
                None => env.push((KEY.into(), value)),
            }
        }
        _ => {}
    }
    Ok(())
}

/// Where a task's servers for Claude Code are written.
pub fn config_file(task: uuid::Uuid) -> PathBuf {
    crate::dirs::state_home().join(format!("soi/mcp/{task}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn finds_own_project_claude_and_opencode_servers() {
        let root = std::env::temp_dir().join(format!("soi-mcp-{}", uuid::Uuid::new_v4()));
        let (home, repo, own) = (root.join("home"), root.join("repo"), root.join("own.json"));
        write(
            &own,
            r#"{"mcpServers":{"github":{"command":"gh-mcp","args":["--ro"]}}}"#,
        );
        write(
            &repo.join(".mcp.json"),
            r#"{"mcpServers":{"github":{"command":"other"},"db":{"command":"db-mcp"}}}"#,
        );
        write(
            &home.join(".claude.json"),
            &format!(
                r#"{{"mcpServers":{{"docs":{{"type":"http","url":"https://docs.example/mcp"}}}},
                    "projects":{{{}:{{"mcpServers":{{"local":{{"command":"local-mcp"}}}}}}}}}}"#,
                serde_json::to_string(&repo).unwrap()
            ),
        );
        write(
            &home.join(".config/opencode/opencode.json"),
            r#"{"mcp":{"jira":{"type":"local","command":["jira-mcp","-v"],"environment":{"TOKEN":"x"}},
                       "odd":{"type":"unknown"}}}"#,
        );
        let servers = discover_in(&home, &own, &repo);
        let found: Vec<(&str, String)> = servers
            .iter()
            .map(|s| (s.name.as_str(), s.description()))
            .collect();
        assert_eq!(
            found,
            [
                ("db", "db-mcp".to_string()),
                ("docs", "https://docs.example/mcp".into()),
                ("github", "gh-mcp --ro".into()),
                ("jira", "jira-mcp -v".into()),
                ("local", "local-mcp".into()),
            ]
        );
        let jira = servers.iter().find(|s| s.name == "jira").unwrap();
        assert_eq!(jira.config["env"]["TOKEN"], "x");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn own_servers_are_checked_before_saving() {
        assert!(check_own(OWN_TEMPLATE).is_ok());
        assert!(check_own(r#"{"mcpServers":{"a":{"url":"https://a"}}}"#).is_ok());
        assert!(check_own("{").is_err());
        assert!(check_own("{}").is_err());
        assert_eq!(
            check_own(r#"{"mcpServers":{"a":{"args":[]}}}"#),
            Err("a needs a \"command\" or a \"url\".".into())
        );
    }

    #[test]
    fn claude_reads_a_file_and_opencode_its_environment() {
        let root = std::env::temp_dir().join(format!("soi-mcp-{}", uuid::Uuid::new_v4()));
        let file = root.join("task.json");
        let servers = [
            Server {
                name: "db".into(),
                config: json!({"command": "db-mcp", "args": ["-v"], "env": {"T": "1"}}),
                path: PathBuf::new(),
            },
            Server {
                name: "docs".into(),
                config: json!({"type": "http", "url": "https://docs.example/mcp"}),
                path: PathBuf::new(),
            },
        ];
        let (mut argv, mut env) = (vec!["claude".to_string()], Vec::new());
        apply("claude", &servers, &file, &mut argv, &mut env).unwrap();
        assert_eq!(argv, ["claude", "--mcp-config", file.to_str().unwrap()]);
        assert!(env.is_empty());
        let written: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(written["mcpServers"]["db"]["args"], json!(["-v"]));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o077, 0);

        let mut argv = vec!["opencode".to_string()];
        let mut env = vec![(
            "OPENCODE_CONFIG_CONTENT".to_string(),
            r#"{"permission":{"external_directory":{"/p/**":"allow"}}}"#.to_string(),
        )];
        apply("opencode", &servers, &file, &mut argv, &mut env).unwrap();
        assert_eq!(argv, ["opencode"]);
        let config: Value = serde_json::from_str(&env[0].1).unwrap();
        assert_eq!(config["permission"]["external_directory"]["/p/**"], "allow");
        assert_eq!(
            config["mcp"]["db"],
            json!({"type": "local", "command": ["db-mcp", "-v"], "environment": {"T": "1"}})
        );
        assert_eq!(
            config["mcp"]["docs"],
            json!({"type": "remote", "url": "https://docs.example/mcp"})
        );

        let mut argv = vec!["pi".to_string()];
        apply("pi", &servers, &file, &mut argv, &mut env).unwrap();
        assert_eq!(argv, ["pi"]);
        fs::remove_dir_all(root).unwrap();
    }
}
