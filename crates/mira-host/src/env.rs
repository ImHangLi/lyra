//! Child environment composition (§7.2): client env → env_files → action env → MIRA_*.

use std::collections::BTreeMap;
use std::path::Path;

use mira_protocol::error::{ErrorCode, ErrorInfo};
use mira_protocol::ipc::ClientEnv;

/// Host-provided variables. Plugin `env` may not override these (validated at load).
#[derive(Debug, Clone, Default)]
pub struct HostVars {
    pub workspace_root: String,
    pub plugin_dir: Option<String>,
    pub state_dir: String,
    pub cache_dir: String,
    pub artifact_dir: String,
    pub run_id: String,
    pub input_file: String,
    pub config_file: String,
}

/// The composed environment. `Debug` lists names only.
#[derive(Clone, Default)]
pub struct ChildEnv(pub BTreeMap<String, String>);

impl std::fmt::Debug for ChildEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ChildEnv({} vars)", self.0.len())
    }
}

/// Reads env files at execution time with dotenvy semantics (no shell). Missing files fail.
pub fn compose(
    client: &ClientEnv,
    root: &Path,
    env_files: &[String],
    action_env: &BTreeMap<String, String>,
    host: &HostVars,
) -> Result<ChildEnv, ErrorInfo> {
    let mut env: BTreeMap<String, String> = client.vars().clone();
    env.retain(|k, _| !k.starts_with("MIRA_"));
    for f in env_files {
        let path = if f.starts_with('/') {
            Path::new(f).to_path_buf()
        } else {
            root.join(f)
        };
        let iter = dotenvy::from_path_iter(&path).map_err(|e| {
            ErrorInfo::new(
                ErrorCode::EXECUTION_FAILED,
                format!("cannot read env file {f}: {e}"),
            )
        })?;
        for item in iter {
            let (k, v) = item.map_err(|e| {
                // A parse error carries the whole line, which may hold a secret value.
                let detail = match e {
                    dotenvy::Error::LineParse(_, at) => format!("cannot parse a line at byte {at}"),
                    other => other.to_string(),
                };
                ErrorInfo::new(
                    ErrorCode::EXECUTION_FAILED,
                    format!("invalid env file {f}: {detail}"),
                )
            })?;
            if !k.starts_with("MIRA_") {
                env.insert(k, v);
            }
        }
    }
    for (k, v) in action_env {
        env.insert(k.clone(), v.clone());
    }
    let mut put = |k: &str, v: &str| {
        env.insert(k.to_owned(), v.to_owned());
    };
    put("MIRA_WORKSPACE_ROOT", &host.workspace_root);
    if let Some(p) = &host.plugin_dir {
        put("MIRA_PLUGIN_DIR", p);
    }
    put("MIRA_STATE_DIR", &host.state_dir);
    put("MIRA_CACHE_DIR", &host.cache_dir);
    put("MIRA_ARTIFACT_DIR", &host.artifact_dir);
    put("MIRA_RUN_ID", &host.run_id);
    put("MIRA_INPUT_FILE", &host.input_file);
    put("MIRA_CONFIG_FILE", &host.config_file);
    Ok(ChildEnv(env))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_malformed_env_line_never_appears_in_the_error() {
        let dir = std::env::temp_dir().join(format!("mira-test-{}-env", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("app.env"), "OK=1\nSECRET_KEY=hunter2 topsecret\n").unwrap();
        let host = HostVars {
            workspace_root: dir.to_string_lossy().into_owned(),
            plugin_dir: None,
            state_dir: String::new(),
            cache_dir: String::new(),
            artifact_dir: String::new(),
            run_id: String::new(),
            input_file: String::new(),
            config_file: String::new(),
        };
        let err = compose(
            &ClientEnv::default(),
            &dir,
            &["app.env".to_owned()],
            &BTreeMap::new(),
            &host,
        )
        .expect_err("a malformed line must fail");
        let shown = serde_json::to_string(&err).unwrap();
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("app.env"), "{shown}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
