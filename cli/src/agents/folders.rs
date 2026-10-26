//! Sidebar folders: named groups of sessions saved in config.json.
//!
//! A session sits in at most one folder; everything else stays in the main
//! list. The orchestrator arranges folders through `difu_sessions`, and the
//! terminal picks up its changes on the next list refresh.
use super::server::Store;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;

pub const MAX_FOLDERS: usize = 64;
const MAX_NAME: usize = 60;
const MAX_MOVE: usize = 100;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub sessions: Vec<String>,
}

/// The folder holding `session`, if any.
pub fn home<'a>(folders: &'a [Folder], session: &str) -> Option<&'a Folder> {
    folders
        .iter()
        .find(|folder| folder.sessions.iter().any(|id| id == session))
}

/// Finds a folder by id, then by case-insensitive name.
pub fn index(folders: &[Folder], key: &str) -> Option<usize> {
    let key = key.trim();
    folders.iter().position(|f| f.id == key).or_else(|| {
        let key = key.to_lowercase();
        folders.iter().position(|f| f.name.to_lowercase() == key)
    })
}
fn resolve(folders: &[Folder], key: &str) -> Result<usize> {
    index(folders, key)
        .with_context(|| format!("No folder named {key:?}; use folders to list them"))
}

fn clean(name: &str) -> Result<String> {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    ensure!(!name.is_empty(), "Folder name cannot be empty");
    ensure!(
        name.chars().count() <= MAX_NAME,
        "Folder names are at most {MAX_NAME} characters"
    );
    Ok(name)
}
fn taken(folders: &[Folder], name: &str, except: Option<usize>) -> bool {
    let name = name.to_lowercase();
    folders
        .iter()
        .enumerate()
        .any(|(i, f)| Some(i) != except && f.name.to_lowercase() == name)
}

/// Adds an empty folder at the end and returns its id.
pub fn create(folders: &mut Vec<Folder>, name: &str) -> Result<String> {
    let name = clean(name)?;
    ensure!(
        folders.len() < MAX_FOLDERS,
        "difu keeps at most {MAX_FOLDERS} folders"
    );
    ensure!(
        !taken(folders, &name, None),
        "A folder named {name:?} already exists"
    );
    let mut base: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .take(32)
        .collect();
    if base.is_empty() {
        base = "folder".into();
    }
    let mut id = base.clone();
    let mut n = 2;
    while folders.iter().any(|f| f.id == id) {
        id = format!("{base}-{n}");
        n += 1;
    }
    folders.push(Folder {
        id: id.clone(),
        name,
        sessions: Vec::new(),
    });
    Ok(id)
}

pub fn rename(folders: &mut [Folder], key: &str, name: &str) -> Result<()> {
    let index = resolve(folders, key)?;
    let name = clean(name)?;
    ensure!(
        !taken(folders, &name, Some(index)),
        "A folder named {name:?} already exists"
    );
    if let Some(folder) = folders.get_mut(index) {
        folder.name = name;
    }
    Ok(())
}

/// Removes the folder; its sessions return to the main list.
pub fn remove(folders: &mut Vec<Folder>, key: &str) -> Result<Folder> {
    let index = resolve(folders, key)?;
    Ok(folders.remove(index))
}

/// Moves sessions into the folder named by `key`, or back to the main list.
pub fn file(folders: &mut [Folder], ids: &[String], key: Option<&str>) -> Result<()> {
    let target = key.map(|key| resolve(folders, key)).transpose()?;
    for folder in folders.iter_mut() {
        folder.sessions.retain(|id| !ids.contains(id));
    }
    if let Some(folder) = target.and_then(|index| folders.get_mut(index)) {
        for id in ids {
            if !folder.sessions.contains(id) {
                folder.sessions.push(id.clone());
            }
        }
    }
    Ok(())
}

/// Moves a folder to `position` (0 is first), clamped to the list.
pub fn place(folders: &mut Vec<Folder>, key: &str, position: usize) -> Result<()> {
    let from = resolve(folders, key)?;
    let folder = folders.remove(from);
    let to = position.min(folders.len());
    folders.insert(to, folder);
    Ok(())
}

/// Swaps a folder with its neighbour; a no-op at either end.
pub fn shift(folders: &mut [Folder], id: &str, down: bool) {
    let Some(from) = folders.iter().position(|f| f.id == id) else {
        return;
    };
    let to = if down {
        from.checked_add(1)
    } else {
        from.checked_sub(1)
    };
    if let Some(to) = to.filter(|to| *to < folders.len()) {
        folders.swap(from, to);
    }
}

/// Drops ids of sessions that no longer exist.
fn prune(folders: &mut [Folder], known: &HashSet<String>) {
    for folder in folders {
        folder.sessions.retain(|id| known.contains(id));
    }
}

fn text(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}
fn required(args: &Value, key: &str) -> Result<String> {
    text(args, key).with_context(|| format!("Missing {key}"))
}
/// Session ids from `ids` (or a single `id`); each must be a coding session.
fn sessions(store: &Store, args: &Value) -> Result<Vec<String>> {
    let mut ids: Vec<String> = match args.get("ids") {
        Some(Value::Array(values)) => values
            .iter()
            .map(|v| v.as_str().map(str::trim).map(String::from))
            .collect::<Option<_>>()
            .context("ids must be a list of session ids")?,
        Some(_) => bail!("ids must be a list of session ids"),
        None => text(args, "id").into_iter().collect(),
    };
    ids.retain(|id| !id.is_empty());
    ids.dedup();
    ensure!(
        ids.len() <= MAX_MOVE,
        "Move at most {MAX_MOVE} sessions at once"
    );
    for id in &ids {
        let orchestrator = store
            .inspect(id, |s| s.orchestrator)
            .with_context(|| format!("Session {id} not found"))?;
        ensure!(
            !orchestrator,
            "Orchestrator chats stay outside folders ({id})"
        );
    }
    Ok(ids)
}

/// Folders with their sessions' ids and titles, for the orchestrator.
fn view(store: &Store, folders: &[Folder]) -> Result<Value> {
    let summaries = store.list()?;
    let listed = |folder: &Folder| {
        let sessions: Vec<_> = folder
            .sessions
            .iter()
            .filter_map(|id| summaries.iter().find(|s| &s.id == id))
            .take(50)
            .map(|s| {
                json!({"id":s.id,"title":super::orchestrator::short(&s.title,80),
                    "status":s.status,"archived":s.archived})
            })
            .collect();
        json!({"id":folder.id,"name":folder.name,"count":folder.sessions.len(),"sessions":sessions})
    };
    let filed: HashSet<&str> = folders
        .iter()
        .flat_map(|f| f.sessions.iter().map(String::as_str))
        .collect();
    let unfiled = summaries
        .iter()
        .filter(|s| s.kind != "Orchestrator" && s.kind != "Guide" && !filed.contains(s.id.as_str()))
        .count();
    Ok(json!({"folders":folders.iter().map(listed).collect::<Vec<_>>(),"unfiled":unfiled}))
}

/// Handles the orchestrator's folder actions; other actions return None.
pub(super) fn handle(store: &Store, args: &Value, action: &str) -> Result<Option<Value>> {
    if !matches!(
        action,
        "folders"
            | "folder_create"
            | "folder_rename"
            | "folder_delete"
            | "folder_move"
            | "folder_reorder"
    ) {
        return Ok(None);
    }
    if action == "folders" {
        let folders = store.storage.load_config()?.session_folders;
        return view(store, &folders).map(Some);
    }
    let known: HashSet<String> = store.list()?.into_iter().map(|s| s.id).collect();
    let mut created = None;
    let config = match action {
        "folder_create" => {
            let name = required(args, "name")?;
            let ids = sessions(store, args)?;
            store.storage.try_update_config(|config| {
                prune(&mut config.session_folders, &known);
                let id = create(&mut config.session_folders, &name)?;
                file(&mut config.session_folders, &ids, Some(&id))?;
                created = Some(id);
                Ok(())
            })?
        }
        "folder_rename" => {
            let key = required(args, "folder")?;
            let name = required(args, "name")?;
            store.storage.try_update_config(|config| {
                prune(&mut config.session_folders, &known);
                rename(&mut config.session_folders, &key, &name)
            })?
        }
        "folder_delete" => {
            let key = required(args, "folder")?;
            store.storage.try_update_config(|config| {
                prune(&mut config.session_folders, &known);
                remove(&mut config.session_folders, &key).map(drop)
            })?
        }
        "folder_move" => {
            let ids = sessions(store, args)?;
            ensure!(!ids.is_empty(), "Missing ids");
            let key = text(args, "folder");
            store.storage.try_update_config(|config| {
                prune(&mut config.session_folders, &known);
                file(&mut config.session_folders, &ids, key.as_deref())
            })?
        }
        _ => {
            let key = required(args, "folder")?;
            let position = args
                .get("position")
                .and_then(Value::as_u64)
                .context("Missing position")?;
            store.storage.try_update_config(|config| {
                prune(&mut config.session_folders, &known);
                place(
                    &mut config.session_folders,
                    &key,
                    usize::try_from(position).unwrap_or(usize::MAX),
                )
            })?
        }
    };
    let mut result = view(store, &config.session_folders)?;
    if let (Some(id), Some(object)) = (created, result.as_object_mut()) {
        object.insert("created".into(), json!(id));
    }
    Ok(Some(result))
}

#[cfg(test)]
mod tests {
    use super::super::{Job, Launch, Session};
    use super::*;

    fn names(folders: &[Folder]) -> Vec<&str> {
        folders.iter().map(|f| f.name.as_str()).collect()
    }
    fn session(id: &str) -> Session {
        Session::new(
            id.into(),
            Job::Coding(Launch {
                repository: "/tmp/repo".into(),
                isolated: false,
                base: String::new(),
                prompt: String::new(),
                model: None,
                effort: None,
            }),
        )
    }

    #[test]
    fn names_are_unique_and_ids_are_stable_slugs() -> Result<()> {
        let mut folders = Vec::new();
        assert_eq!(create(&mut folders, "  Widget   bugs ")?, "widget-bugs");
        assert!(create(&mut folders, "widget BUGS").is_err());
        assert!(create(&mut folders, "   ").is_err());
        assert_eq!(create(&mut folders, "!!!")?, "folder");
        assert_eq!(create(&mut folders, "???")?, "folder-2");
        rename(&mut folders, "widget-bugs", "Widget")?;
        assert_eq!(names(&folders), ["Widget", "!!!", "???"]);
        assert_eq!(folders.first().map(|f| f.id.as_str()), Some("widget-bugs"));
        assert!(rename(&mut folders, "widget", "???").is_err());
        Ok(())
    }

    #[test]
    fn a_session_lives_in_one_folder_and_deleting_a_folder_keeps_sessions() -> Result<()> {
        let mut folders = Vec::new();
        create(&mut folders, "One")?;
        create(&mut folders, "Two")?;
        let ids = ["a".to_owned(), "b".to_owned()];
        file(&mut folders, &ids, Some("one"))?;
        file(&mut folders, &ids[..1], Some("TWO"))?;
        assert_eq!(home(&folders, "a").map(|f| f.id.as_str()), Some("two"));
        assert_eq!(home(&folders, "b").map(|f| f.id.as_str()), Some("one"));
        file(&mut folders, &ids[1..], None)?;
        assert!(home(&folders, "b").is_none());
        assert!(file(&mut folders, &ids, Some("missing")).is_err());
        assert_eq!(home(&folders, "a").map(|f| f.id.as_str()), Some("two"));
        let removed = remove(&mut folders, "Two")?;
        assert_eq!(removed.sessions, ["a"]);
        assert!(home(&folders, "a").is_none());
        Ok(())
    }

    #[test]
    fn folders_reorder_by_position_and_by_neighbour() -> Result<()> {
        let mut folders = Vec::new();
        for name in ["A", "B", "C"] {
            create(&mut folders, name)?;
        }
        place(&mut folders, "c", 0)?;
        assert_eq!(names(&folders), ["C", "A", "B"]);
        place(&mut folders, "C", 99)?;
        assert_eq!(names(&folders), ["A", "B", "C"]);
        shift(&mut folders, "a", false);
        assert_eq!(names(&folders), ["A", "B", "C"]);
        shift(&mut folders, "a", true);
        assert_eq!(names(&folders), ["B", "A", "C"]);
        shift(&mut folders, "c", true);
        assert_eq!(names(&folders), ["B", "A", "C"]);
        Ok(())
    }

    #[test]
    fn orchestrator_arranges_folders_and_cannot_file_itself() -> Result<()> {
        let home_dir = tempfile::tempdir()?;
        let mut manager = session(super::super::orchestrator::ID);
        manager.orchestrator = true;
        let store = Store::fixture(
            home_dir.path().into(),
            vec![manager, session("one"), session("two")],
        );
        let created = handle(
            &store,
            &json!({"name":"Widget","ids":["one"]}),
            "folder_create",
        )?
        .context("handled")?;
        assert_eq!(created.get("created"), Some(&json!("widget")));
        assert_eq!(created.get("unfiled"), Some(&json!(1)));
        handle(
            &store,
            &json!({"folder":"widget","ids":["two"]}),
            "folder_move",
        )?;
        let saved = store.storage.load_config()?.session_folders;
        assert_eq!(
            saved.first().map(|f| f.sessions.clone()),
            Some(vec!["one".to_owned(), "two".to_owned()])
        );
        let refused = handle(
            &store,
            &json!({"folder":"widget","id":super::super::orchestrator::ID}),
            "folder_move",
        );
        assert!(refused.is_err());
        assert!(
            handle(
                &store,
                &json!({"folder":"widget","id":"missing"}),
                "folder_move"
            )
            .is_err()
        );
        handle(&store, &json!({"id":"one"}), "folder_move")?;
        handle(
            &store,
            &json!({"folder":"Widget","name":"Widget bugs"}),
            "folder_rename",
        )?;
        let listed = handle(&store, &json!({}), "folders")?.context("handled")?;
        assert_eq!(
            listed.pointer("/folders/0/name"),
            Some(&json!("Widget bugs"))
        );
        assert_eq!(listed.pointer("/folders/0/count"), Some(&json!(1)));
        handle(&store, &json!({"folder":"widget bugs"}), "folder_delete")?;
        assert!(store.storage.load_config()?.session_folders.is_empty());
        assert!(handle(&store, &json!({}), "list")?.is_none());
        Ok(())
    }

    #[test]
    fn deleted_sessions_are_pruned_on_the_next_change() -> Result<()> {
        let home_dir = tempfile::tempdir()?;
        let store = Store::fixture(home_dir.path().into(), vec![session("one")]);
        store.storage.update_config(|config| {
            config.session_folders = vec![Folder {
                id: "old".into(),
                name: "Old".into(),
                sessions: vec!["gone".into(), "one".into()],
            }];
        })?;
        handle(&store, &json!({"name":"New"}), "folder_create")?;
        let saved = store.storage.load_config()?.session_folders;
        assert_eq!(
            saved.first().map(|f| f.sessions.clone()),
            Some(vec!["one".to_owned()])
        );
        Ok(())
    }
}
