# First setup

Goal: a small set of ordinary plugins that run this project's real commands, validated and applied, so the human can open `lyra` and use them immediately.

1. **Workspace.** Use the directory the user named. Otherwise `lyra setup --json` picks it (`data.selected_root`, `data.reason`). If `selected_root` is null, ask the user once, listing `data.candidates`. Never scan outside the project.
2. **Existing state.** If `.lyra/workspace.json` exists, read `lyra catalog --json` first and extend it; never overwrite `.lyra/local.json` (personal overrides).
3. **Facts.** `lyra setup --json` returns static facts only (`kind`, `value`, `source.path`, `source.lines`, `certainty`), plus `ambiguities` and `truncated`. Nothing was executed. Treat `hint` facts as leads, not truth.
4. **Meaning.** Read the README/CONTRIBUTING files and the real scripts the facts point to. Decide what each command does from its source, not from its name. A `test` or `migrate` target can have side effects.
5. **Plugins.** Create a few tools that are useful on day one: the dev server(s), required dependencies (e.g. Compose services the docs say to start), and the common checks. Usually one `dev` plugin with command actions is enough. See the lyra-extend skill for the manifest format. Do not change the Lyra Core.
6. **Per action, decide:** `mode` (`task` ends, `process` keeps running), `cwd`, whether it needs a real terminal (`terminal: "pty"` only for interactive programs), the stop signal and grace, and a `cleanup` command for resources the process starts outside its own process group (for example detached containers). Never use `down -v` or any data-deleting cleanup by default.
7. **Write and apply.** Put files in `.lyra/.drafts/setup/` (a `.lyra`-shaped directory: `workspace.json` + `plugins/<id>/plugin.json`), then:
   ```sh
   lyra validate .lyra/.drafts/setup --json
   lyra apply .lyra/.drafts/setup --expected-revision 0 --json   # use the current catalog_revision if one exists
   ```
   Add ignore rules for personal and generated files to the project's `.gitignore` without rewriting it: `.lyra/local.json`, `.lyra/.generated/`, `.lyra/.drafts/`.
8. **Verify.** Run one quick task (`lyra run dev.check`) and, if the user agrees, start the main service and read its logs. Stop what you started unless the user wants it running.
9. **Report** in a few lines: the tools created (refs), what was verified to start, what was not verified (missing credentials, services not tried), and how to stop anything still running. Tell the user to open `lyra` for the TUI.

Ask the user only for credentials, real ambiguity, or preferences you cannot infer. Do not ask which framework or package manager they use when the facts already say it.
