//! Collections of skills and commands to request together (New task
//! worktree, Task details), as set up in Settings ▸ Agents. One may be the
//! default, chosen for every new task. Kept in
//! `~/.config/soi/collections.json`.

use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    /// What the dropdowns show.
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    /// By name without the `/`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
    /// Chosen for new tasks; at most one is.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub default: bool,
}

fn path() -> PathBuf {
    crate::dirs::config_home().join("soi/collections.json")
}

pub fn load() -> Vec<Collection> {
    fs::read_to_string(path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save(collections: &[Collection]) -> io::Result<()> {
    let path = path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(collections).map_err(io::Error::other)?;
    fs::write(path, json + "\n")
}

/// Makes the one at `index` the default, or none with `None`.
pub fn set_default(collections: &mut [Collection], index: Option<usize>) {
    for (i, collection) in collections.iter_mut().enumerate() {
        collection.default = Some(i) == index;
    }
}

/// The default's name, if there is one.
pub fn default_name(collections: &[Collection]) -> Option<String> {
    Some(collections[default_index(collections)?].name.clone())
}

/// Where the default is, if there is one.
pub fn default_index(collections: &[Collection]) -> Option<usize> {
    collections.iter().position(|c| c.default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_default_and_round_trip() {
        let mut list = vec![
            Collection {
                name: "Backend".into(),
                skills: vec!["tdd".into()],
                commands: vec!["review".into()],
                default: true,
            },
            Collection {
                name: "Docs".into(),
                ..Default::default()
            },
        ];
        set_default(&mut list, Some(1));
        assert_eq!(default_index(&list), Some(1));
        assert!(!list[0].default);
        set_default(&mut list, None);
        assert_eq!(default_index(&list), None);
        let json = serde_json::to_string(&list).unwrap();
        assert_eq!(
            serde_json::from_str::<Vec<Collection>>(&json).unwrap(),
            list
        );
        assert!(!json.contains("default"));
    }
}
