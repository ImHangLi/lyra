# Onboarding verification (LYR-15)

Real projects, real agents, one unchanged `mira` binary. Recorded on 2026-09-26, macOS 27.2 on Apple Silicon (arm64), zsh, Docker Desktop 29.4.3. Agents were headless Claude Code runs (`claude -p`) limited to local tools. Unless a step says otherwise, they were not allowed to install dependencies, start Docker, or run package managers.

Results use four labels: **pass**, **fail**, **blocked**, and **not-run**. "Structure" means the plugins validate and apply. "Startup" means the real commands ran through Mira. "macOS interaction" means TUI use in a real PTY.

## Projects

| Project | Stack | Commit | Structure | Startup | macOS interaction |
|---|---|---|---|---|---|
| [FastAPI full-stack template](https://github.com/fastapi/full-stack-fastapi-template) | Python (uv) + Compose + Vite | `cb740b656d7a` | pass | pass (db, mailpit, migrations, backend) | pass |
| [Outline](https://github.com/outline/outline) | TypeScript (yarn) + Makefile + Compose | `ed4aaf274df8` | pass | not-run: dependencies not installed (yarn is not on this machine, and a full install is several GB) | not-run |
| `tests/fixtures/workspace` | own fixture | this repo | pass | pass | pass |

The plugins the agents generated are kept as examples in `examples/` (drafts, discovery caches, and personal files excluded).

## Real repositories before v0.2.0

Agents set up Mira on these repositories by following the installed skills literally. They used the v0.1.1 build plus the fixes that were not yet released. Their findings were fixed in #48, #49, and #50.

| Project | Agent | Setup time | Tools | Verified live |
|---|---|---|---|---|
| [nextjs/saas-starter](https://github.com/nextjs/saas-starter) (Next.js, Postgres, Drizzle) | Claude, skills loaded | about 6 min | 10, including a structured routes table | Postgres through Compose, migration, `next dev` (`/` 200), stop with cleanup |
| [pallets/flask](https://github.com/pallets/flask) | generic agent that reads `.agents/skills/mira/SKILL.md` | about 2 min to the first tools | 11, including a pytest report table with a row action | tests, a failing test (exit 5 and a readable traceback), an example app as a service |


## Use cases

| Check | Persona | Result | Evidence |
|---|---|---|---|
| First setup from a clean checkout (FastAPI) | A/H | pass | The agent read the skill, ran `mira setup --json`, and read the docs and scripts. It then wrote 9 `dev` actions: Compose services with a `docker compose stop` cleanup (never `down -v`), prestart, backend, frontend, lint, tests, and prek. It validated and applied them and appended 3 ignore rules. It reported what it could not verify. 14 turns. |
| First setup (Outline) | A/H | pass (structure) | 10 actions came from the Makefile, package scripts, and Compose. `test-db-prepare` is labeled as recreating the test database only. |
| Discovery never executes the project | A/F | pass | `setup --json` gave 81 facts (FastAPI) and 148 (Outline); both git trees stayed clean. The Makefile's `test` and `destroy` targets were reported, not run. |
| Start services, migrate, serve (FastAPI) | H/A | pass | `start dev.services` brought up postgres:18 and mailpit, and `run dev.prestart` ran the migrations and initial data in 5 s. `start dev.backend` then served `/api/v1/utils/health-check/` with `200 true`. |
| Human and agent share one run | M | pass | The CLI (agent) started `dev.backend`. The TUI (human) showed the same run ID with live logs, and `s` in the TUI stopped it. The CLI read the run as `cancelled`, stop reason `user`. |
| Save a one-off command as a tool (UC-02) | A/E | pass | Session A worked the answer out once with `mira exec … python3 -c '<ast script>'`. It then saved `api-routes.list` with an `api-routes.endpoints` table and 2 optional inputs, validated, and applied (revision 3). |
| A new agent reuses it without the old chat | A | pass | Session B was asked only "which endpoints does users expose?". It found `api-routes.list` through the catalog, ran it, and answered 10 endpoints in 6 turns and 22 s. It created no new plugin. VALUE-03. |
| Add a typed panel live, without changing Core | E/H | pass | Asked for a table of Compose services and ports, the agent wrote `compose-ports` (manifest plus Python). It validated, applied, and ran it; the rows match the Compose files. Bad input gave SCHEMA_INVALID. The `mira` binary was unchanged. VALUE-04. |
| Two worktrees, fixed port conflict, existing containers | W/F | pass | The second worktree got its own workspace ID. Its `dev.services` failed on port 5432 (exit 1), and its cleanup stopped only `fastapi-wt2-*`. The first worktree's containers kept running. An unrelated container kept running throughout. All 15 pre-existing containers on the machine were unchanged. LIFE-05. |
| Normal end keeps data | W/F | pass | `mira down` took 0.8 s. The run was cancelled by `session_closed` and its cleanup succeeded. The containers are stopped, not removed, and the `app-db-data` volumes of both worktrees still exist. |
| Historical versus current | A/H | pass | Views and runs from finished runs read `historical`, with the source run and time. Runs whose definition changed read `stale` with `definition_current: false` (see #31). VALUE-06. |
| Agent update via publish (UC-10) | A/H | pass (CLI) | A `publish` to `agent-updates.feed` with a request key was saved with no session running, and a new host shows it as historical (see #25). No hook integration was tested. |

## Limits

- Only Claude Code was run as the agent. Codex and generic agents use the same skill files but were not tested.
- Outline was validated structurally only. The frontend (`bun`) and the full Compose stack of the FastAPI template were not started.
- One agent's environment had a security-review hook, which made the agent tighten an input pattern. This record reports that behavior; the agent's other actions were unaffected.
- The agent runs show the workflow. They are not measurements of productivity.
