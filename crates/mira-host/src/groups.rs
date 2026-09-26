//! A small on-disk ledger of live child process groups, so work a crashed host left running can
//! be stopped by the next host. Each entry records the group leader's PID and start time; a
//! group is signalled only while that exact process still leads it, never a reused PID.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use mira_protocol::ids::RunId;
use rustix::process::{Pid, Signal, kill_process_group};

static DIR: OnceLock<PathBuf> = OnceLock::new();

/// Sets the ledger directory (once per host process) and creates it with mode 0700.
pub fn init(dir: PathBuf) {
    use std::os::unix::fs::DirBuilderExt;
    let _ = std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir);
    let _ = DIR.set(dir);
}

fn entry(run_id: &RunId) -> Option<PathBuf> {
    DIR.get().map(|d| d.join(run_id.as_str()))
}

/// The start time of `pid` as `ps` prints it, or `None` when it is gone. Together with the
/// PID this identifies one process: a reused PID has a later start time, and `exec` (which
/// can change the command name) keeps it.
fn identity(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    let out = std::process::Command::new("/bin/ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && !text.is_empty()).then_some(text)
}

/// Records a spawned group leader. Blocking (it runs `ps`); failures are ignored because the
/// ledger is a safety net.
pub fn record(run_id: &RunId, pid: u32) {
    let (Some(path), Some(id)) = (entry(run_id), identity(pid)) else {
        return;
    };
    let _ = std::fs::write(path, format!("{pid}\n{id}\n"));
}

/// Records the group from a blocking task, so a spawn never waits for `ps`.
pub fn record_soon(run_id: &RunId, pid: u32) {
    let run_id = run_id.clone();
    tokio::task::spawn_blocking(move || record(&run_id, pid));
}

/// Removes the entry once the run's processes have ended.
pub fn forget(run_id: &RunId) {
    if let Some(path) = entry(run_id) {
        let _ = std::fs::remove_file(path);
    }
}

fn read(path: &Path) -> Option<(u32, String)> {
    let text = std::fs::read_to_string(path).ok()?;
    let (pid, id) = text.split_once('\n')?;
    Some((pid.trim().parse().ok()?, id.trim().to_owned()))
}

/// Sends SIGKILL to the recorded group when its leader is still the recorded process.
/// Returns the PID when a group was signalled.
fn kill_if_same(path: &Path) -> Option<u32> {
    let (pid, id) = read(path)?;
    if identity(pid).as_deref() != Some(id.as_str()) {
        return None;
    }
    let p = Pid::from_raw(i32::try_from(pid).ok()?)?;
    kill_process_group(p, Signal::KILL).ok().map(|()| pid)
}

/// At host start: stops every group a previous host left behind and clears the ledger.
/// Returns `(run id, pid)` for each group that was stopped.
pub fn reap_leftovers() -> Vec<(String, u32)> {
    let Some(dir) = DIR.get() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut stopped = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        if let Some(pid) = kill_if_same(&path) {
            stopped.push((e.file_name().to_string_lossy().into_owned(), pid));
        }
        let _ = std::fs::remove_file(&path);
    }
    stopped
}

/// At shutdown: SIGKILL for the groups of runs that did not stop within the deadline.
pub fn kill_remaining<'a>(runs: impl IntoIterator<Item = &'a RunId>) {
    for run_id in runs {
        if let Some(path) = entry(run_id) {
            kill_if_same(&path);
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::{CommandExt, ExitStatusExt};

    use super::*;

    #[test]
    fn identity_is_stable_for_a_live_process() {
        let pid = std::process::id();
        assert!(identity(pid).is_some());
        assert_eq!(identity(pid), identity(pid));
    }

    #[test]
    fn a_mismatched_entry_is_never_signalled() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let path = std::env::temp_dir().join(format!("mira-test-{}-group", std::process::id()));
        std::fs::write(&path, format!("{}\nThu Jan  1 00:00:00 1970\n", child.id())).unwrap();
        assert_eq!(kill_if_same(&path), None);
        assert!(
            child.try_wait().unwrap().is_none(),
            "the process must still run"
        );
        std::fs::write(
            &path,
            format!("{}\n{}\n", child.id(), identity(child.id()).unwrap()),
        )
        .unwrap();
        assert_eq!(kill_if_same(&path), Some(child.id()));
        assert!(child.wait().unwrap().signal().is_some());
        let _ = std::fs::remove_file(path);
    }
}
