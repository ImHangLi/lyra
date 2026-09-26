# Release and install verification (LYR-17)

`scripts/release/package.sh` builds `lyra-<version>-<target>.tar.gz` and a `.sha256` file for each target into `dist/`. `scripts/install.sh` installs one archive into `~/.lyra/bin`: it needs no sudo and no Node. Recorded on 2026-09-26, macOS 27.2 on Apple Silicon.

| Check | Result | Evidence |
|---|---|---|
| Build arm64 and x86_64 archives | pass | `aarch64-apple-darwin` (Mach-O arm64, 6.1 MB) and `x86_64-apple-darwin` (Mach-O x86_64, 6.6 MB), each with its checksum file |
| Fresh install into a clean `HOME` | pass | Ran with `env -i HOME=… LYRA_INSTALL_FROM=dist`. The checksum was verified and `lyra --version` printed `lyra 0.1.0`. The installer then shows the PATH hint, the next step, and how to uninstall |
| Reinstall over its own install | pass | Replaced the binary using the install marker |
| A `lyra` that this installer did not install | pass | Refused with exit 1, and the existing file was not touched |
| Corrupted download | pass | `checksum mismatch; nothing was installed`, and the installed binary was byte-identical before and after |
| Another `lyra` first on PATH | pass | The installer warned: "another lyra is first on PATH" |
| Gatekeeper and quarantine | pass | The script never runs `xattr`, `spctl`, or any other quarantine tool. The binary is ad-hoc linker-signed only. There is no Developer ID signature and no notarization |
| First use in a project that is not set up | pass | `status` works. `catalog` returns NOT_SETUP with the next step `lyra setup --json`. `lyra` with no TTY returns TTY_REQUIRED. In a PTY, `lyra` shows one setup screen. `skills install --agent claude` wrote the skills, and the agent reads `.claude/skills/lyra/SKILL.md` |
| Launch the x86_64 build | not-run | This Mac has no Rosetta and no Intel machine was available |
| Install from a real GitHub release | blocked | No release was published. Publishing is an outward release step that needs the owner's approval. After a release exists, the command is `curl -fsSL https://raw.githubusercontent.com/ImHangLi/lyra/main/scripts/install.sh \| sh` |
| Clean macOS user account | not-run | A clean `HOME` was used instead of a new macOS account |
