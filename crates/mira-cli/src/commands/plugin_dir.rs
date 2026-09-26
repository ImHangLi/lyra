//! One plugin folder checked against, or applied to, the workspace's `.mira`.
//!
//! The CLI builds a temporary `.mira`-shaped draft: the current `workspace.json` (with the
//! plugin's entry added when it is new) and every relative plugin folder, where the given
//! folder takes the place of `plugins/<id>`. The host applies that draft as usual.

use std::path::{Component, Path, PathBuf};

use mira_protocol::config::{self, ConfigSet, LoadedPlugin, PLUGIN_FILE, WORKSPACE_FILE};
use mira_protocol::error::{ErrorCode, ErrorInfo, Issue, Issues};
use serde_json::Value;

/// A directory with `plugin.json` and no `workspace.json`.
pub fn is_plugin_dir(path: &Path) -> bool {
    path.is_dir() && path.join(PLUGIN_FILE).is_file() && !path.join(WORKSPACE_FILE).is_file()
}

/// `3 tools`: the actions and views of one plugin.
pub fn tools(p: &LoadedPlugin) -> String {
    let n = p.plugin.actions.len() + p.plugin.views.len();
    format!("{n} tool{}", if n == 1 { "" } else { "s" })
}

/// A temporary draft; removed when dropped.
pub struct PluginDraft {
    dir: PathBuf,
    mira_dir: PathBuf,
}

impl Drop for PluginDraft {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn invalid(message: impl Into<String>) -> ErrorInfo {
    ErrorInfo::new(ErrorCode::INVALID_ARGUMENT, message)
}

fn io(what: &Path, e: std::io::Error) -> ErrorInfo {
    ErrorInfo::new(ErrorCode::INTERNAL, format!("{}: {e}", what.display()))
}

impl PluginDraft {
    /// Builds the draft for `plugin` on top of `mira_dir` as it is on disk now.
    pub fn build(mira_dir: &Path, plugin: &LoadedPlugin) -> Result<Self, ErrorInfo> {
        let mira = std::fs::canonicalize(mira_dir).map_err(|e| io(mira_dir, e))?;
        let ws_file = mira.join(WORKSPACE_FILE);
        let ws_text = std::fs::read_to_string(&ws_file).map_err(|e| io(&ws_file, e))?;
        let ws: Value = serde_json::from_str(&ws_text).map_err(|e| {
            ErrorInfo::new(
                ErrorCode::SCHEMA_INVALID,
                format!("{}: {e}", ws_file.display()),
            )
        })?;
        let mut entries: Vec<String> = ws
            .get("plugins")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();

        let candidate = PathBuf::from(plugin.dir.as_str());
        if mira.starts_with(&candidate) {
            return Err(invalid(format!(
                "{} contains .mira; give the plugin folder itself",
                candidate.display()
            )));
        }
        // Inside .mira the folder is its own entry; outside it is copied to plugins/<id>.
        let (entry, source) = if let Ok(rel) = candidate.strip_prefix(&mira) {
            (rel.to_string_lossy().into_owned(), None)
        } else if let Some(abs) = entries
            .iter()
            .find(|e| e.starts_with('/') && std::fs::canonicalize(e).is_ok_and(|c| c == candidate))
        {
            (abs.clone(), None)
        } else {
            (format!("plugins/{}", plugin.plugin.id), Some(candidate))
        };
        let added = !entries.contains(&entry);
        if added {
            entries.push(entry.clone());
        }
        if let Some(e) = entries.iter().find(|e| {
            !e.starts_with('/') && Path::new(e).components().any(|c| c == Component::ParentDir)
        }) {
            return Err(invalid(format!(
                "workspace.json lists `{e}`, outside .mira; apply a full .mira draft instead"
            )));
        }

        let draft = Self {
            dir: make_temp_dir()?,
            mira_dir: mira.clone(),
        };
        for e in entries.iter().filter(|e| !e.starts_with('/')) {
            let src = match &source {
                Some(s) if *e == entry => s.clone(),
                _ => mira.join(e),
            };
            if src.is_dir() {
                copy_tree(&src, &draft.dir.join(e))?;
            }
        }
        let text = if added {
            add_entry(&ws_text, &entry).unwrap_or_else(|| {
                let mut ws = ws;
                if let Some(a) = ws.get_mut("plugins").and_then(Value::as_array_mut) {
                    a.push(Value::String(entry.clone()));
                }
                serde_json::to_string_pretty(&ws).unwrap_or_default() + "\n"
            })
        } else {
            ws_text
        };
        let target = draft.dir.join(WORKSPACE_FILE);
        std::fs::write(&target, text).map_err(|e| io(&target, e))?;
        Ok(draft)
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// Validates the whole draft, as the host will. Issue files name `.mira`, not the draft.
    pub fn check(&self) -> Result<ConfigSet, ErrorInfo> {
        let local = config::load_local(&self.mira_dir).map_err(|i| i.to_error_info())?;
        config::load_config_dir(&self.dir, local.as_ref())
            .map_err(|issues| self.in_mira(issues).to_error_info())
    }

    fn in_mira(&self, issues: Issues) -> Issues {
        let mut prefixes = vec![self.dir.clone()];
        if let Ok(c) = std::fs::canonicalize(&self.dir) {
            prefixes.push(c);
        }
        let map = |i: Issue| -> Issue {
            let file = i.file.as_ref().and_then(|f| {
                prefixes.iter().find_map(|p| {
                    Path::new(f)
                        .strip_prefix(p)
                        .ok()
                        .map(|rel| self.mira_dir.join(rel).to_string_lossy().into_owned())
                })
            });
            Issue {
                file: file.or(i.file),
                ..i
            }
        };
        Issues(issues.0.into_iter().map(map).collect())
    }
}

fn make_temp_dir() -> Result<PathBuf, ErrorInfo> {
    use std::os::unix::fs::DirBuilderExt;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    let dir = std::env::temp_dir().join(format!("mira-plugin-{}-{nanos}", std::process::id()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .map_err(|e| io(&dir, e))?;
    Ok(dir)
}

/// Copies files and folders (not symlinks, as the host's apply does).
fn copy_tree(src: &Path, dst: &Path) -> Result<(), ErrorInfo> {
    std::fs::create_dir_all(dst).map_err(|e| io(dst, e))?;
    for e in std::fs::read_dir(src).map_err(|e| io(src, e))?.flatten() {
        let (from, to) = (e.path(), dst.join(e.file_name()));
        match e.file_type() {
            Ok(t) if t.is_dir() => copy_tree(&from, &to)?,
            Ok(t) if t.is_file() => {
                std::fs::copy(&from, &to).map_err(|e| io(&from, e))?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Adds `entry` to the top-level `plugins` array of `text`, keeping the rest of the file.
fn add_entry(text: &str, entry: &str) -> Option<String> {
    let b = text.as_bytes();
    let (mut depth, mut i) = (0usize, 0usize);
    let mut key: Option<&str> = None;
    let mut want = false;
    let mut open: Option<usize> = None;
    while i < b.len() {
        match b[i] {
            b'"' => {
                let start = i + 1;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                if depth == 1 && open.is_none() {
                    key = text.get(start..i);
                }
            }
            b':' if depth == 1 && open.is_none() => want = key == Some("plugins"),
            b'[' => {
                if want && depth == 1 && open.is_none() {
                    open = Some(i);
                }
                depth += 1;
            }
            b'{' => depth += 1,
            b']' | b'}' => {
                depth = depth.checked_sub(1)?;
                if let (Some(o), 1, b']') = (open, depth, b[i]) {
                    let inner = &text[o + 1..i];
                    let quoted = serde_json::to_string(entry).ok()?;
                    let out = if inner.trim().is_empty() {
                        format!("{}{quoted}{}", &text[..=o], &text[i..])
                    } else {
                        let end = o + 1 + inner.trim_end().len();
                        format!("{}, {quoted}{}", &text[..end], &text[end..])
                    };
                    let v: Value = serde_json::from_str(&out).ok()?;
                    let last = v.get("plugins")?.as_array()?.last()?.as_str()?;
                    return (last == entry).then_some(out);
                }
            }
            b',' if depth == 1 => {
                key = None;
                want = false;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_entry_keeps_the_file_shape() {
        let ws = "{\n  \"name\": \"plugins\",\n  \"plugins\": [\"plugins/dev\"],\n  \"meta\": {\"plugins\": []}\n}\n";
        let out = add_entry(ws, "plugins/errors").unwrap();
        assert_eq!(
            out,
            "{\n  \"name\": \"plugins\",\n  \"plugins\": [\"plugins/dev\", \"plugins/errors\"],\n  \"meta\": {\"plugins\": []}\n}\n"
        );
        let empty = "{\"api\": 1, \"plugins\": [ ]}";
        assert_eq!(
            add_entry(empty, "plugins/x").unwrap(),
            "{\"api\": 1, \"plugins\": [\"plugins/x\"]}"
        );
        assert_eq!(add_entry("{\"api\": 1}", "plugins/x"), None);
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn plugin_json(id: &str) -> String {
        format!(
            r#"{{"api": 1, "id": "{id}", "name": "{id}", "description": "Test.", "actions": [{{"id": "check", "title": "Check", "description": "Check.", "mode": "task", "run": {{"kind": "command", "argv": ["true"]}}}}]}}"#
        )
    }

    #[test]
    fn plugin_draft_adds_new_plugins_and_rejects_duplicate_ids() {
        let root = make_temp_dir().unwrap();
        let mira = root.join(".mira");
        write(
            &mira.join(WORKSPACE_FILE),
            r#"{"api": 1, "name": "t", "plugins": ["plugins/dev"]}"#,
        );
        write(
            &mira.join("plugins/dev").join(PLUGIN_FILE),
            &plugin_json("dev"),
        );

        // A new plugin outside .mira goes to plugins/<id>.
        write(&root.join("new").join(PLUGIN_FILE), &plugin_json("errors"));
        let p = config::load_plugin_dir(&root.join("new"), None).unwrap();
        assert_eq!(tools(&p), "1 tool");
        let d = PluginDraft::build(&mira, &p).unwrap();
        assert!(d.path().join("plugins/errors").join(PLUGIN_FILE).is_file());
        let set = d.check().unwrap();
        assert_eq!(set.plugins.len(), 2);

        // Replacing an existing plugin by the same id is fine.
        write(&root.join("dev2").join(PLUGIN_FILE), &plugin_json("dev"));
        let p = config::load_plugin_dir(&root.join("dev2"), None).unwrap();
        assert_eq!(
            PluginDraft::build(&mira, &p)
                .unwrap()
                .check()
                .unwrap()
                .plugins
                .len(),
            1
        );

        // A second folder inside .mira with a listed id is a duplicate.
        write(
            &mira.join("plugins/dev-copy").join(PLUGIN_FILE),
            &plugin_json("dev"),
        );
        let p = config::load_plugin_dir(&mira.join("plugins/dev-copy"), None).unwrap();
        let d = PluginDraft::build(&mira, &p).unwrap();
        let e = d.check().unwrap_err();
        assert!(e.message.contains("duplicate plugin id"), "{}", e.message);
        assert!(e.message.contains("/.mira/workspace.json"), "{}", e.message);
        let draft_dir = d.path().to_path_buf();
        drop(d);
        assert!(!draft_dir.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
