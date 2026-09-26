//! Child environment composition (§7.2): client env → env_files → action env → LYRA_*.

use std::collections::BTreeMap;
use std::path::Path;

use lyra_protocol::error::{ErrorCode, ErrorInfo};
use lyra_protocol::ipc::ClientEnv;

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
    env.retain(|k, _| !k.starts_with("LYRA_"));
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
            if !k.starts_with("LYRA_") {
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
    put("LYRA_WORKSPACE_ROOT", &host.workspace_root);
    if let Some(p) = &host.plugin_dir {
        put("LYRA_PLUGIN_DIR", p);
    }
    put("LYRA_STATE_DIR", &host.state_dir);
    put("LYRA_CACHE_DIR", &host.cache_dir);
    put("LYRA_ARTIFACT_DIR", &host.artifact_dir);
    put("LYRA_RUN_ID", &host.run_id);
    put("LYRA_INPUT_FILE", &host.input_file);
    put("LYRA_CONFIG_FILE", &host.config_file);
    Ok(ChildEnv(env))
}
