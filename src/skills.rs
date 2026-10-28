//! Agent skills and commands found on disk, to request for a task. A skill
//! is a folder holding a `SKILL.md` whose frontmatter names and describes
//! it; a command (Claude Code's slash commands, Codex's and Pi's prompts) is
//! a Markdown file named after it. Only read, never changed.
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Skill,
    Command,
}

impl Kind {
    /// Folders for the user, then for the project in a repository, as
    /// Claude Code, Codex, OpenCode and Pi keep them; then the folder in a
    /// Claude Code plugin.
    fn folders(
        self,
    ) -> (
        &'static [&'static str],
        &'static [&'static str],
        &'static str,
    ) {
        match self {
            Self::Skill => (
                &[
                    ".claude/skills",
                    ".agents/skills",
                    ".codex/skills",
                    ".config/opencode/skill",
                    ".config/opencode/skills",
                    ".pi/agent/skills",
                ],
                &[
                    ".claude/skills",
                    ".agents/skills",
                    ".codex/skills",
                    ".opencode/skill",
                    ".opencode/skills",
                    ".pi/skills",
                ],
                "skills",
            ),
            Self::Command => (
                &[
                    ".claude/commands",
                    ".codex/prompts",
                    ".config/opencode/command",
                    ".config/opencode/commands",
                    ".pi/agent/prompts",
                ],
                &[
                    ".claude/commands",
                    ".opencode/command",
                    ".opencode/commands",
                    ".pi/prompts",
                ],
                "commands",
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

/// The skills or commands available for work in `repository`, by name;
/// where two share a name, the project's wins.
pub fn discover(kind: Kind, repository: &Path) -> Vec<Item> {
    discover_in(kind, &crate::dirs::home_dir(), repository)
}

pub fn discover_in(kind: Kind, home: &Path, repository: &Path) -> Vec<Item> {
    let (user, project, plugin_folder) = kind.folders();
    let mut items: Vec<Item> = Vec::new();
    let folders = project
        .iter()
        .map(|dir| (repository.join(dir), None))
        .chain(user.iter().map(|dir| (home.join(dir), None)))
        .chain(
            claude_plugins(home)
                .into_iter()
                .map(|(name, dir)| (dir.join(plugin_folder), Some(name))),
        );
    for (folder, plugin) in folders {
        let found = match kind {
            Kind::Skill => skills_in(&folder),
            Kind::Command => commands_in(&folder, "", 0),
        };
        for mut item in found {
            // Claude Code calls a plugin's `plugin:name`.
            if let Some(plugin) = &plugin {
                item.name = format!("{plugin}:{}", item.name);
            }
            if !items.iter().any(|i| i.name == item.name) {
                items.push(item);
            }
        }
    }
    items.sort_by_key(|item| item.name.to_lowercase());
    items
}

/// Installed Claude Code plugins' folders, with their names.
fn claude_plugins(home: &Path) -> Vec<(String, PathBuf)> {
    let path = home.join(".claude/plugins/installed_plugins.json");
    let Some(json) = fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
    else {
        return Vec::new();
    };
    let Some(plugins) = json.get("plugins").and_then(Value::as_object) else {
        return Vec::new();
    };
    plugins
        .iter()
        .flat_map(|(id, installs)| {
            let name = id.split('@').next().unwrap_or(id).to_string();
            installs
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|install| install.get("installPath")?.as_str())
                .map(move |path| (name.clone(), PathBuf::from(path)))
        })
        .collect()
}

fn skills_in(folder: &Path) -> Vec<Item> {
    let Ok(entries) = fs::read_dir(folder) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path().join("SKILL.md");
            let text = fs::read_to_string(&path).ok()?;
            let fallback = entry.file_name().to_string_lossy().into_owned();
            let (name, description) = frontmatter(&text);
            Some(Item {
                name: name.unwrap_or(fallback),
                description: description.unwrap_or_default(),
                path,
            })
        })
        .collect()
}

const MAX_COMMAND_DEPTH: usize = 8;

/// The `.md` files in `folder`, named after the file; those in subfolders
/// get the folder in front, as Claude Code does: `frontend/test.md` is
/// `frontend:test`. Subfolders only go so deep, so linked folders that loop
/// end.
fn commands_in(folder: &Path, prefix: &str, depth: usize) -> Vec<Item> {
    let Ok(entries) = fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut items = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if stem.is_empty() || stem.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            if depth < MAX_COMMAND_DEPTH {
                items.extend(commands_in(&path, &format!("{prefix}{stem}:"), depth + 1));
            }
        } else if path.extension().is_some_and(|e| e == "md")
            && let Ok(text) = fs::read_to_string(&path)
        {
            items.push(Item {
                name: format!("{prefix}{stem}"),
                description: frontmatter(&text).1.unwrap_or_default(),
                path,
            });
        }
    }
    items
}

/// `name` and `description` from a `SKILL.md`'s YAML frontmatter; only the
/// plain one-line values skills use.
fn frontmatter(text: &str) -> (Option<String>, Option<String>) {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (None, None);
    }
    let (mut name, mut description) = (None, None);
    for line in lines.take_while(|line| line.trim() != "---") {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches(['"', '\'']).trim();
        if value.is_empty() {
            continue;
        }
        match key.trim() {
            "name" => name = Some(value.to_string()),
            "description" => description = Some(value.to_string()),
            _ => {}
        }
    }
    (name, description)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn finds_user_project_and_plugin_skills() {
        let root = std::env::temp_dir().join(format!("soi-skills-{}", uuid::Uuid::new_v4()));
        let (home, repo) = (root.join("home"), root.join("repo"));
        write(
            &home.join(".agents/skills/tdd/SKILL.md"),
            "---\nname: tdd\ndescription: \"Test first.\"\n---\n# TDD\n",
        );
        write(
            &home.join(".claude/skills/review/SKILL.md"),
            "---\nname: review\ndescription: User's\n---\n",
        );
        write(
            &repo.join(".claude/skills/review/SKILL.md"),
            "---\nname: review\ndescription: Project's\n---\n",
        );
        write(
            &home.join(".claude/skills/plain/SKILL.md"),
            "# No frontmatter\n",
        );
        write(&home.join(".claude/skills/synced/notes.md"), "not a skill");
        let plugin = root.join("plugin");
        write(
            &plugin.join("skills/setup/SKILL.md"),
            "---\nname: setup\ndescription: Sets up.\n---\n",
        );
        write(
            &home.join(".claude/plugins/installed_plugins.json"),
            &format!(
                r#"{{"plugins":{{"tools@market":[{{"installPath":{}}}]}}}}"#,
                serde_json::to_string(&plugin).unwrap()
            ),
        );
        let skills = discover_in(Kind::Skill, &home, &repo);
        let found: Vec<(&str, &str)> = skills
            .iter()
            .map(|s| (s.name.as_str(), s.description.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                ("plain", ""),
                ("review", "Project's"),
                ("tdd", "Test first."),
                ("tools:setup", "Sets up."),
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn finds_commands_with_their_folders() {
        let root = std::env::temp_dir().join(format!("soi-commands-{}", uuid::Uuid::new_v4()));
        let (home, repo) = (root.join("home"), root.join("repo"));
        write(
            &home.join(".claude/commands/review.md"),
            "---\ndescription: Review the diff\n---\nReview $ARGUMENTS\n",
        );
        write(
            &home.join(".claude/commands/frontend/test.md"),
            "Run the tests\n",
        );
        write(&home.join(".claude/commands/notes.txt"), "not a command");
        write(&repo.join(".claude/commands/review.md"), "Project review\n");
        write(&home.join(".codex/prompts/explain.md"), "Explain\n");
        let found: Vec<(String, String)> = discover_in(Kind::Command, &home, &repo)
            .into_iter()
            .map(|i| (i.name, i.description))
            .collect();
        assert_eq!(
            found,
            [
                ("explain".to_string(), String::new()),
                ("frontend:test".into(), String::new()),
                ("review".into(), String::new()),
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }
}
