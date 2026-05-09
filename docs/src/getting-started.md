# Getting Started

## Prerequisites

The workspace is pinned to the toolchain declared in `rust-toolchain.toml` and the `justfile`.

Common commands:

```bash
just format
just lint
just test
just docs-build
```

## Bootstrap a repo

The fastest way to bootstrap a repository is now:

```bash
polyphony init
```

This command:

- creates `WORKFLOW.md` when missing
- creates `~/.config/polyphony/config.toml` when missing
- seeds `.polyphony/agents/` with starter prompt files
- seeds `polyphony.toml` in git repositories when local tracker/workspace wiring can be inferred
- accepts explicit pack parameters for tracker, repository, project slug, and default branch
- auto-detects GitHub and GitLab remotes for repo-local tracker wiring
- prints setup hints when a starter still needs tracker or auth edits
- auto-detects supported local agent CLIs and inserts their profiles into the user config on first
  run

Starter packs are built in:

```bash
polyphony init --list-packs
polyphony init --pack codex
polyphony init --pack multi-agent
polyphony init --pack pipeline-static
polyphony init --pack pipeline-planner
polyphony init --pack automation-feedback
polyphony init --pack pipeline-static --tracker github --repository owner/repo
polyphony init --pack codex --tracker linear --project-slug ENG
```

Use `--force` to overwrite an existing `WORKFLOW.md` with the selected pack. `--template` and
`--list-templates` still work as backward-compatible aliases.

Pack-by-pack guidance lives in [Starter Templates](./starter-templates.md).

## Running polyphony

If you skip `polyphony init`, the normal runtime still bootstraps missing files on first start. The
generated `~/.config/polyphony/config.toml` is an annotated reference template, with every
supported top-level config area shown and local CLI terminal settings such as `use_tmux`
documented inline. The default values keep `tracker.kind = "none"` and no dispatch agents, so the
real CLI can start without external services or mock data:

```bash
cargo run -p polyphony-cli
```

Or install a release build into `~/.local/bin`:

```bash
just install
```

If the repo already ships a generic shared `WORKFLOW.md`, the CLI also seeds
`.polyphony/config.toml` so you can point workspaces back at the current repository without
editing the checked-in workflow.

Configure GitHub or Linear in `.polyphony/config.toml` when you want the dashboard to show real
issues for the current repo. Leave `agents.profiles` empty in `~/.config/polyphony/config.toml`
for tracker-only mode.

If you work across multiple repos with different trackers, keep shared tracker credentials in
`~/.config/polyphony/config.toml` under `trackers.profiles.<name>`, then select one in
`.polyphony/config.toml` with `tracker.profile = "<name>"`.

For public GitHub repositories, issue polling can work without `GITHUB_TOKEN`, but authenticated
requests are still recommended to avoid low anonymous rate limits.

Starter references for the generated files live in `templates/WORKFLOW.md`,
`templates/config.toml`, and `templates/repo-config.toml`. Copyable full-file examples live under
`templates/examples/`.

Run without the TUI:

```bash
cargo run -p polyphony-cli -- --no-tui
```

Enable SQLite persistence:

```bash
cargo run -p polyphony-cli --features sqlite -- --sqlite-url sqlite://polyphony.db
```

## Building this book

This documentation uses `mdBook`.

Build the static site:

```bash
just docs-build
```

Serve it locally with live reload:

```bash
just docs-serve
```
