# Release and install verification (LYR-17)

`scripts/release/package.sh` builds `mira-<version>-<target>.tar.gz` and a `.sha256` file for each target into `dist/`. `scripts/install.sh` installs one archive into `~/.mira/bin`: it needs no sudo and no Node. Recorded on 2026-09-26, macOS 27.2 on Apple Silicon.

| Check | Result | Evidence |
|---|---|---|
| Build arm64 and x86_64 archives | pass | `aarch64-apple-darwin` (Mach-O arm64, 6.1 MB) and `x86_64-apple-darwin` (Mach-O x86_64, 6.6 MB), each with its checksum file. v0.1.0 ships the arm64 archive only |
| Fresh install into a clean `HOME` | pass | Ran with `env -i HOME=… MIRA_INSTALL_FROM=dist`. The checksum was verified and `mira --version` printed `mira 0.1.0`. The installer then shows the PATH hint, the next step, and how to uninstall |
| Reinstall over its own install | pass | Replaced the binary using the install marker |
| A `mira` that this installer did not install | pass | Refused with exit 1, and the existing file was not touched |
| Corrupted download | pass | `checksum mismatch; nothing was installed`, and the installed binary was byte-identical before and after |
| Another `mira` first on PATH | pass | The installer warned: "another mira is first on PATH" |
| Gatekeeper and quarantine | pass | The script never runs `xattr`, `spctl`, or any other quarantine tool. The binary is ad-hoc linker-signed only. There is no Developer ID signature and no notarization |
| First use in a project that is not set up | pass | `status` works. `catalog` returns NOT_SETUP with the next step `mira setup --json`. `mira` with no TTY returns TTY_REQUIRED. In a PTY, `mira` shows one setup screen. `skills install --agent claude` wrote the skills, and the agent reads `.claude/skills/mira/SKILL.md` |
| Launch the x86_64 build | not-run | Out of scope: every target machine is Apple Silicon (M3 or later). No x86_64 archive is published |
| Install from a real GitHub release | pass | [v0.1.0](https://github.com/ImHangLi/mira/releases/tag/v0.1.0) (tag on `039c0c3`, CI green). In a clean `HOME` with a minimal `PATH`, `curl -fsSL https://raw.githubusercontent.com/ImHangLi/mira/main/scripts/install.sh \| sh` found the latest release, verified the checksum, and installed `mira 0.1.0`. The installed binary is byte-identical to the release build and has no `com.apple.quarantine` attribute. It links only system libraries (CoreFoundation, CoreServices, libiconv, libSystem), and its minimum macOS is 11.0 |
| First use of the released binary | pass | On a clone of the `v0.1.0` tag: `validate`, `run dev.check`, `run markers.scan`, `view markers.table` (marked historical with its source run), `up --background`, `start dev.web`, `logs`, and `down --wait`. In a new project: `status`, `catalog` gives NOT_SETUP, `skills install` for `claude`, `codex`, and `generic`, and `setup --json`. The TUI was rendered in a PTY: the home screen, a new search after an earlier filter, Esc restoring that filter, a run with logs, and help |
| Clean macOS user account | not-run | A clean `HOME` was used instead of a new macOS account |
