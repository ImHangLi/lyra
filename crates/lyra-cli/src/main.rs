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
    /// Bounded tool catalog.
    Catalog {
        #[arg(long)]
        search: Option<String>,
        /// Return not_modified when the catalog revision still equals N.
        #[arg(long, value_name = "N")]
        if_revision: Option<u64>,
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Purpose, inputs, and invocation hints of one plugin.item.
    Describe {
        #[arg(value_name = "REF")]
        item: String,
        #[arg(long)]
        include_schema: bool,
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
    /// Internal: serve the workspace host.
    #[command(name = "__host", hide = true)]
    Host {
        #[arg(long)]
        root: PathBuf,
    },
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
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
            let message = e.render().to_string();
            let first = message
                .lines()
                .next()
                .unwrap_or("invalid arguments")
                .trim_start_matches("error: ");
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
            limit,
        }) => commands::inspect::catalog(&ctx, search, if_revision, limit),
        Some(Command::Describe {
            item,
            include_schema,
        }) => commands::inspect::describe(&ctx, item, include_schema),
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
        }) => commands::runtime::runs(&ctx, run, action, outcome, limit, after),
        Some(Command::Logs {
            target,
            after,
            limit,
            max_bytes,
            follow,
        }) => commands::runtime::logs(&ctx, &target, after, limit, max_bytes, follow),
        Some(Command::Paths) => commands::inspect::paths(&ctx),
        Some(Command::Doctor) => commands::inspect::doctor(&ctx),
        Some(Command::Host { root }) => match lyra_protocol::ids::AbsolutePath::from_path(&root) {
            Ok(root) => ExitCode::from(lyra_host::run(root)),
            Err(_) => ExitCode::from(2),
        },
        Some(Command::Schema { name }) => commands::contract::schema(mode, &name),
        Some(Command::Validate { path }) => commands::contract::validate(mode, &path),
        None => {
            use clap::CommandFactory;
            let _ = Cli::command().print_help();
            ExitCode::SUCCESS
        }
    }
}
