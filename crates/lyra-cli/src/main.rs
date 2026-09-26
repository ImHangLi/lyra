//! `lyra`: one binary for the human TUI and the agent CLI.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use lyra_protocol::reply::ReplyContext;

mod commands;
mod output;

use output::Mode;

#[derive(Parser)]
#[command(
    name = "lyra",
    version,
    about = "Agent-native control plane for your development workflow."
)]
struct Cli {
    /// Select the workspace explicitly.
    #[arg(long, global = true, value_name = "PATH")]
    project: Option<PathBuf>,
    /// Print exactly one JSON reply object (default when stdout is not a TTY).
    #[arg(long, global = true, conflicts_with = "text")]
    json: bool,
    /// Print human-readable text.
    #[arg(long, global = true)]
    text: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Print a raw JSON Schema: workspace, plugin, local, invocation, plugin-event, cli-reply, ipc.
    Schema { name: String },
    /// Validate a draft directory, workspace.json, or plugin.json without executing anything.
    Validate {
        #[arg(value_name = "DRAFT_DIR")]
        path: PathBuf,
    },
    /// Current session, active runs, and storage/config warnings.
    Status,
    /// Bounded tool catalog (default 30 items within 32 KiB; continue with --after).
    Catalog {
        #[arg(long)]
        search: Option<String>,
        /// Return not_modified when the catalog revision still equals N.
        #[arg(long, value_name = "N")]
        if_revision: Option<u64>,
        /// With --if-revision: the workspace ID the cached catalog came from.
        #[arg(long, value_name = "WORKSPACE_ID", requires = "if_revision")]
        if_workspace: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
        /// Continue from the previous reply's meta.next_cursor.
        #[arg(long, value_name = "CURSOR")]
        after: Option<String>,
        /// Reply budget in bytes, envelope included (default 32 KiB, up to 256 KiB).
        #[arg(long)]
        max_bytes: Option<u32>,
    },
    /// Purpose, inputs, and invocation hints of one plugin.item.
    Describe {
        #[arg(value_name = "REF")]
        item: String,
        #[arg(long)]
        include_schema: bool,
        /// Reply budget; schemas that do not fit are returned by payload reference.
        #[arg(long)]
        max_bytes: Option<u32>,
    },
    /// This workspace's config, state, log, cache, and runtime locations.
    Paths,
    /// Check the Core, configuration, socket, and required executables.
    Doctor,
    /// Run a task; waits for the result unless --no-wait (which needs a session).
    Run {
        #[arg(value_name = "ACTION")]
        action: String,
        /// JSON object input file, or `-` for stdin.
        #[arg(long, value_name = "FILE")]
        input: Option<String>,
        #[arg(long)]
        no_wait: bool,
        #[arg(long, value_name = "KEY")]
        request_key: Option<String>,
    },
    /// Start a process, or reuse the running instance with the same input.
    Start {
        #[arg(value_name = "ACTION")]
        action: String,
        #[arg(long, value_name = "FILE")]
        input: Option<String>,
        #[arg(long, value_name = "KEY")]
        request_key: Option<String>,
    },
    /// Stop one managed run (by run ID or action ref).
    Stop {
        #[arg(value_name = "RUN_OR_ACTION")]
        target: String,
        /// Wait until the run and its cleanup finished.
        #[arg(long)]
        wait: bool,
    },
    /// Stop the action's current run, then start it with the current definition.
    Restart {
        #[arg(value_name = "ACTION")]
        action: String,
        #[arg(long, value_name = "FILE")]
        input: Option<String>,
    },
    /// Run an ad-hoc command through the managed task path (not saved to the catalog).
    Exec {
        #[arg(long)]
        label: String,
        #[arg(long, value_name = "KEY")]
        request_key: Option<String>,
        #[arg(last = true, required = true, value_name = "ARGV")]
        argv: Vec<String>,
    },
    /// Keep a session running without an open TUI, until the TTL or `lyra down`.
    Up {
        #[arg(long)]
        background: bool,
        #[arg(long, default_value = "2h")]
        ttl: String,
    },
    /// Keep the existing session running in the background.
    Keep {
        #[arg(long, default_value = "2h")]
        ttl: String,
    },
    /// Stop the workspace session and the work it owns (no data is deleted).
    Down {
        #[arg(long)]
        wait: bool,
    },
    /// Recent runs, or one run's details.
    Runs {
        #[arg(value_name = "RUN")]
        run: Option<String>,
        #[arg(long, value_name = "REF")]
        action: Option<String>,
        #[arg(long)]
        outcome: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long, value_name = "CURSOR")]
        after: Option<String>,
        #[arg(long)]
        max_bytes: Option<u32>,
    },
    /// Bounded run output: the tail of the current or latest run by default.
    Logs {
        #[arg(value_name = "RUN_OR_ACTION")]
        target: String,
        #[arg(long, value_name = "CURSOR")]
        after: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        max_bytes: Option<u32>,
        #[arg(long)]
        follow: bool,
    },
    /// Read one page of a view with its source, revision, and freshness (never re-runs it).
    View {
        #[arg(value_name = "VIEW")]
        view: String,
        #[arg(long, value_name = "CURSOR")]
        after: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        max_bytes: Option<u32>,
    },
    /// Publish one LPP view frame to VIEW; needs no session for `last` views.
    Publish {
        #[arg(value_name = "VIEW")]
        view: String,
        /// The frame file, or `-` for stdin.
        #[arg(long, value_name = "FILE")]
        input: String,
        #[arg(long, value_name = "N")]
        expected_view_revision: Option<u64>,
        #[arg(long, value_name = "KEY")]
        request_key: Option<String>,
    },
    /// Run a table row action with input bound from that row at the given view revision.
    ViewAction {
        #[arg(value_name = "VIEW")]
        view: String,
        #[arg(value_name = "ACTION")]
        action: String,
        #[arg(long, value_name = "ROW")]
        row: String,
        #[arg(long, value_name = "N")]
        expected_view_revision: u64,
    },
    /// List registered artifacts (optionally of one run), or read one as bounded text.
    #[command(args_conflicts_with_subcommands = true)]
    Artifacts {
        #[arg(value_name = "RUN")]
        run: Option<String>,
        #[command(subcommand)]
        read: Option<ArtifactsCommand>,
    },
    /// Read host-held payloads referenced by meta.payload or result.payload.
    Payload {
        #[command(subcommand)]
        command: PayloadCommand,
    },
    /// Accept a draft definition set (validate first; the catalog revision must match).
    Apply {
        #[arg(value_name = "DRAFT_DIR")]
        draft: PathBuf,
        /// The catalog revision your change is based on.
        #[arg(long, value_name = "N")]
        expected_revision: u64,
        #[arg(long, value_name = "KEY")]
        request_key: Option<String>,
    },
    /// Re-validate `.lyra` from disk; accept it only when valid.
    Reload,
    /// Turn an action's interval schedule on or off (runs only inside a session).
    Schedule {
        #[arg(value_name = "ACTION")]
        action: String,
        #[arg(value_enum)]
        switch: commands::schedule::Switch,
    },
    /// Read the screen of an interactive (PTY) run as plain text with its screen revision.
    ///
    /// This is the current virtual screen, not a log; `lyra logs RUN` holds the transcript.
    /// Agent flow: `lyra up --background`, `lyra run ACTION --no-wait` for a PTY action, then
    /// alternate `lyra terminal RUN` and `lyra input RUN ...` until the program finishes.
    #[command(verbatim_doc_comment)]
    Terminal {
        #[arg(value_name = "RUN")]
        run: String,
        /// Reply byte budget (default 32 KiB); rows past it are cut and meta.truncated is set.
        #[arg(long, value_name = "N")]
        max_bytes: Option<u32>,
    },
    /// Type into an interactive (PTY) run; prints the screen after the program reacts.
    ///
    /// Usage: lyra input RUN (--text TEXT | --key KEY) [--expected-screen-revision N]
    ///
    /// Inside `input`, --text is the input text, not the output-mode flag; output is JSON
    /// unless stdout is a terminal.
    /// Each call takes the input lock for that one write and fails with INPUT_BUSY while
    /// another client (for example the TUI) holds it. --text is sent as UTF-8 exactly as
    /// given and never adds Enter; send `--key enter` separately.
    /// Keys: enter, tab, escape, backspace, delete, up, down, left, right, ctrl-c, ctrl-d,
    /// ctrl-z, ctrl-right-bracket.
    /// With --expected-screen-revision N nothing is written and SCREEN_CHANGED is returned
    /// when the screen moved past revision N (read it with `lyra terminal RUN`).
    /// Example: lyra run dev.prompt --no-wait; lyra terminal RUN;
    ///          lyra input RUN --text Ada; lyra input RUN --key enter
    #[command(verbatim_doc_comment)]
    Input {
        #[arg(value_name = "RUN")]
        run: String,
        /// Given as `--text TEXT` (see the usage above).
        #[arg(
            long = "input-text",
            value_name = "TEXT",
            conflicts_with = "key",
            required_unless_present = "key",
            hide = true
        )]
        input_text: Option<String>,
        /// One named key (see the list above).
        #[arg(long, value_name = "KEY")]
        key: Option<String>,
        /// Refuse to write unless the screen is still at revision N.
        #[arg(long, value_name = "N")]
        expected_screen_revision: Option<u64>,
    },
    /// Storage usage, retention cleanup, and explicit private-state clearing.
    Storage {
        #[command(subcommand)]
        command: StorageCommand,
    },
    /// Install or update the bundled agent skills in this workspace.
    Skills {
        #[command(subcommand)]
        command: SkillsCommand,
    },
    /// Internal: serve the workspace host.
    #[command(name = "__host", hide = true)]
    Host {
        #[arg(long)]
        root: PathBuf,
    },
    /// Report bounded, read-only project facts for the setup skill; never runs project code.
    Setup {
        /// Rescan even when the discovery cache is still valid.
        #[arg(long)]
        refresh: bool,
    },
}

#[derive(Subcommand)]
enum ArtifactsCommand {
    /// Read a bounded UTF-8 text chunk of one artifact.
    Read {
        #[arg(value_name = "ID")]
        id: String,
        #[arg(long, default_value_t = 0)]
        offset: u64,
        #[arg(long)]
        max_bytes: Option<u32>,
    },
}

/// `lyra input RUN --text TEXT` (§10.2) spells its text option like the global `--text`
/// output flag. After the `input` subcommand, `--text` means the input text.
fn rewrite_input_text(args: Vec<String>) -> Vec<String> {
    // With `--key`, or as the last argument, `--text` is the output-mode flag.
    let has_key = args.iter().any(|a| a == "--key" || a.starts_with("--key="));
    let mut out = Vec::with_capacity(args.len());
    let mut iter = args.into_iter().peekable();
    out.extend(iter.next());
    let mut in_input = false;
    while let Some(a) = iter.next() {
        if a == "--" {
            out.push(a);
            out.extend(iter);
            break;
        }
        if !in_input {
            let project = a == "--project";
            let positional = !a.starts_with('-');
            out.push(a);
            if project {
                out.extend(iter.next());
            } else if positional {
                if out.last().is_some_and(|s| s == "input") {
                    in_input = true;
                } else {
                    out.extend(iter);
                    break;
                }
            }
            continue;
        }
        match a.strip_prefix("--text") {
            Some("") if has_key || iter.peek().is_none() => out.push(a),
            Some("") => out.push("--input-text".to_owned()),
            Some(v) if v.starts_with('=') => out.push(format!("--input-text{v}")),
            _ => out.push(a),
        }
    }
    out
}

#[derive(Subcommand)]
enum PayloadCommand {
    /// Read one bounded UTF-8 chunk (default 16 KiB); never re-runs the producing action.
    Read {
        #[arg(value_name = "TOKEN")]
        token: String,
        /// JSON Pointer selecting a subvalue first, e.g. /rows/0.
        #[arg(long)]
        pointer: Option<String>,
        /// Byte offset of the selected serialization; continue with next_offset.
        #[arg(long, default_value_t = 0)]
        offset: u64,
        #[arg(long)]
        max_bytes: Option<u32>,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum GcKindArg {
    Cache,
    Logs,
    History,
    Artifacts,
    All,
}

#[derive(Subcommand)]
enum StorageCommand {
    /// Managed usage by category and budget; `--all` also lists other workspaces (observe only).
    Status {
        #[arg(long)]
        all: bool,
    },
    /// Plan retention cleanup; `--apply` deletes what the current policy allows.
    Gc {
        #[arg(long, value_enum, default_value = "all")]
        kind: GcKindArg,
        #[arg(long)]
        apply: bool,
    },
    /// Delete one plugin's private state (only when it has no active run).
    Clear {
        #[arg(long)]
        plugin: String,
        /// Only `state` is supported.
        #[arg(long, default_value = "state")]
        kind: String,
    },
}

#[derive(Subcommand)]
enum SkillsCommand {
    /// Copy `lyra` and `lyra-extend` into .agents/skills (and .claude/skills for Claude); never overwrites your edits.
    Install {
        #[arg(long, value_enum, default_value = "generic")]
        agent: commands::skills::AgentKind,
    },
}

fn main() -> ExitCode {
    let args = rewrite_input_text(std::env::args().collect());
    let cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(e) => {
            use clap::error::ErrorKind;
            if matches!(
                e.kind(),
                ErrorKind::DisplayHelp
                    | ErrorKind::DisplayVersion
                    | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            ) {
                let _ = e.print();
                return ExitCode::SUCCESS;
            }
            let mode = Mode::resolve(
                args.iter().any(|a| a == "--json"),
                args.iter().any(|a| a == "--text"),
            );
            if mode == Mode::Text {
                let _ = e.print();
                return ExitCode::from(2);
            }
            // Keep the details (such as the missing flag names) but not the usage block.
            let message = e.render().to_string();
            let first = message
                .lines()
                .take_while(|l| !l.starts_with("Usage:") && !l.starts_with("For more information"))
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
                .trim_start_matches("error: ")
                .replace("--input-text", "--text");
            let first = if first.is_empty() {
                "invalid arguments".to_owned()
            } else {
                first
            };
            return output::fail(
                mode,
                ReplyContext::default(),
                output::invalid_argument(first),
            );
        }
    };
    let mode = Mode::resolve(cli.json, cli.text);
    let ctx = commands::ctx::Ctx {
        mode,
        project: cli.project.clone(),
    };
    match cli.command {
        Some(Command::Status) => commands::inspect::status(&ctx),
        Some(Command::Catalog {
            search,
            if_revision,
            if_workspace,
            limit,
            after,
            max_bytes,
        }) => commands::inspect::catalog(
            &ctx,
            commands::inspect::CatalogArgs {
                search,
                if_revision,
                if_workspace,
                limit,
                after,
                max_bytes,
            },
        ),
        Some(Command::Describe {
            item,
            include_schema,
            max_bytes,
        }) => commands::inspect::describe(&ctx, item, include_schema, max_bytes),
        Some(Command::Run {
            action,
            input,
            no_wait,
            request_key,
        }) => commands::runtime::run(&ctx, &action, input.as_deref(), no_wait, request_key),
        Some(Command::Start {
            action,
            input,
            request_key,
        }) => commands::runtime::start(&ctx, &action, input.as_deref(), request_key),
        Some(Command::Stop { target, wait }) => commands::runtime::stop(&ctx, &target, wait),
        Some(Command::Restart { action, input }) => {
            commands::runtime::restart(&ctx, &action, input.as_deref())
        }
        Some(Command::Exec {
            label,
            request_key,
            argv,
        }) => commands::runtime::exec(&ctx, label, argv, request_key),
        Some(Command::Up { background, ttl }) => commands::runtime::up(&ctx, background, &ttl),
        Some(Command::Keep { ttl }) => commands::runtime::keep(&ctx, &ttl),
        Some(Command::Down { wait }) => commands::runtime::down(&ctx, wait),
        Some(Command::Runs {
            run,
            action,
            outcome,
            limit,
            after,
            max_bytes,
        }) => commands::runtime::runs(&ctx, run, action, outcome, limit, after, max_bytes),
        Some(Command::Logs {
            target,
            after,
            limit,
            max_bytes,
            follow,
        }) => commands::runtime::logs(&ctx, &target, after, limit, max_bytes, follow),
        Some(Command::View {
            view,
            after,
            limit,
            max_bytes,
        }) => commands::views::view(&ctx, &view, after, limit, max_bytes),
        Some(Command::Publish {
            view,
            input,
            expected_view_revision,
            request_key,
        }) => commands::views::publish(&ctx, &view, &input, expected_view_revision, request_key),
        Some(Command::ViewAction {
            view,
            action,
            row,
            expected_view_revision,
        }) => commands::views::view_action(&ctx, &view, &action, row, expected_view_revision),
        Some(Command::Artifacts { run, read: None }) => commands::views::artifacts(&ctx, run),
        Some(Command::Artifacts {
            read:
                Some(ArtifactsCommand::Read {
                    id,
                    offset,
                    max_bytes,
                }),
            ..
        }) => commands::views::artifact_read(&ctx, id, offset, max_bytes),
        Some(Command::Terminal { run, max_bytes }) => {
            commands::terminal::terminal(&ctx, &run, max_bytes)
        }
        Some(Command::Input {
            run,
            input_text: text,
            key,
            expected_screen_revision,
        }) => commands::terminal::input(&ctx, &run, text, key, expected_screen_revision),
        Some(Command::Payload {
            command:
                PayloadCommand::Read {
                    token,
                    pointer,
                    offset,
                    max_bytes,
                },
        }) => commands::payload::read(&ctx, token, pointer, offset, max_bytes),
        Some(Command::Paths) => commands::inspect::paths(&ctx),
        Some(Command::Doctor) => commands::inspect::doctor(&ctx),
        Some(Command::Host { root }) => match lyra_protocol::ids::AbsolutePath::from_path(&root) {
            Ok(root) => ExitCode::from(lyra_host::run(root)),
            Err(_) => ExitCode::from(2),
        },
        Some(Command::Schema { name }) => commands::contract::schema(mode, &name),
        Some(Command::Validate { path }) => commands::contract::validate(mode, &path),
        Some(Command::Apply {
            draft,
            expected_revision,
            request_key,
        }) => commands::config::apply(&ctx, &draft, expected_revision, request_key),
        Some(Command::Schedule { action, switch }) => {
            commands::schedule::set(&ctx, &action, switch)
        }
        Some(Command::Storage { command }) => match command {
            StorageCommand::Status { all } => commands::storage::status(&ctx, all),
            StorageCommand::Gc { kind, apply } => {
                use lyra_protocol::ipc::GcKind;
                let kind = match kind {
                    GcKindArg::Cache => GcKind::Cache,
                    GcKindArg::Logs => GcKind::Logs,
                    GcKindArg::History => GcKind::History,
                    GcKindArg::Artifacts => GcKind::Artifacts,
                    GcKindArg::All => GcKind::All,
                };
                commands::storage::gc(&ctx, kind, apply)
            }
            StorageCommand::Clear { plugin, kind } if kind == "state" => {
                commands::storage::clear(&ctx, &plugin)
            }
            StorageCommand::Clear { .. } => output::fail(
                mode,
                ReplyContext::default(),
                output::invalid_argument("only --kind state is supported"),
            ),
        },
        Some(Command::Skills {
            command: SkillsCommand::Install { agent },
        }) => commands::skills::install(&ctx, agent),
        Some(Command::Reload) => commands::config::reload(&ctx),
        Some(Command::Setup { refresh }) => {
            commands::setup::run(mode, ctx.project.as_deref(), refresh)
        }
        None => commands::tui::open(&ctx),
    }
}
