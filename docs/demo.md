# Four-minute demo

The demo shows the main path: a project launchpad (UC-01), then saving a one-off command as a tool (UC-02), then the human and the agent using the same run. Label any sped-up segment as sped up. Keep real repositories and fixtures apart: the FastAPI template is the real project, and the playground is a fixture.

**Preparation** (not recorded):
- Check out the FastAPI full-stack template at the pinned commit.
- Pull the Compose images and run `uv sync` in `backend/`.
- Install Lyra and run `lyra skills install --agent claude`.
- Keep a terminal with the agent and a second terminal for `lyra` side by side.

| Time | Show | Say |
|---|---|---|
| 0:00–0:25 | Many terminal tabs, then one `lyra` window | Engineers have plenty of tools. What they lack is one place where people and agents see, keep, and reuse their work. |
| 0:25–1:05 | Ask the agent: "Set up Lyra for this repository." Show `lyra setup --json` facts, the agent writing `.lyra/plugins/dev`, then `validate` and `apply` | Setup uses your existing agent. Lyra only reads facts; it never runs the project to guess. The result is ordinary plugin files. |
| 1:05–1:50 | In the TUI, start `dev.services` and `dev.backend`. Ask the agent: "is the backend healthy?" It reads `lyra logs dev.backend` for the same run ID | One host per workspace. The agent reads the same run the human started. There is no second instance and no hidden shell. |
| 1:50–2:40 | Ask the agent: "list the API endpoints." It uses `lyra exec` once, then saves the command as `api-routes.list` with a table | A command that worked once becomes a tool, with inputs and a table, and no Core change. |
| 2:40–3:20 | A new agent session is asked the question again. It finds `api-routes.list` in the catalog and answers. The human opens the same table in the TUI | The next agent reuses the tool instead of rediscovering it. The human and the agent see the same data. |
| 3:20–3:50 | Close the TUI; the services stop and the Compose volumes remain. Reopen it: the history reads `historical`, with the run and time | Closing the last window stops the work it owns, and no data is deleted. Old results never pretend to be current. |
| 3:50–4:00 | The README install line | One native binary, no Node. Your agent sets it up. |

**Numbers you may quote**, all measured (see `docs/verification/performance.md`, Apple M5 Pro, release build):
- warm TUI first frame p95 11 ms;
- navigation p95 3 ms;
- `lyra status` p95 7.5 ms.

Do not present them as results on other hardware.
