//! The workspace's git branch for the header. Reads `.git` files directly (the same way the
//! host records a run's git context) and never spawns `git`.

use std::path::{Path, PathBuf};

/// The checked-out branch, or the short commit when HEAD is detached. `None` when the
/// workspace is not a git repository or HEAD cannot be read.
pub fn head_label(root: &Path) -> Option<String> {
    let dot = root.join(".git");
    let gitdir = if dot.is_file() {
        // A worktree or submodule: `.git` names the real git directory.
        let text = std::fs::read_to_string(&dot).ok()?;
        let p = PathBuf::from(text.strip_prefix("gitdir:")?.trim());
        if p.is_absolute() { p } else { root.join(p) }
    } else if dot.is_dir() {
        dot
    } else {
        return None;
    };
    let head = std::fs::read_to_string(gitdir.join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref: ") {
        Some(r) => Some(r.strip_prefix("refs/heads/").unwrap_or(r).to_owned()),
        None if !head.is_empty() => Some(head.chars().take(7).collect()),
        None => None,
    }
}
