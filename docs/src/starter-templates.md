# Starter Templates

`polyphony init` ships with a small set of starter workflow packs so a repo can begin from a
concrete shape instead of editing the full reference template from scratch.

List them from the CLI:

```bash
polyphony init --list-packs
```

Seed common tracker values directly during init:

```bash
polyphony init --pack pipeline-static --tracker github --repository owner/repo
polyphony init --pack codex --tracker linear --project-slug ENG
polyphony init --pack multi-agent --tracker gitlab --repository group/project
```

You can also set `--default-branch main` to seed the repo-local workspace default branch in
`polyphony.toml`. `--template` and `--list-templates` still work as compatibility aliases.

## `default`

The full annotated reference template.

Use it when:

- you want the broadest configuration surface visible up front
- you expect to customize the workflow heavily
- you prefer editing from a reference file rather than starting opinionated

## `codex`

Single-agent Codex workflow using the legacy `codex:` shorthand.

Use it when:

- Codex is your main engine
- you want the shortest path from install to first dispatch
- you do not need multi-agent routing yet

## `multi-agent`

Multi-provider routing example with explicit agent profiles and fallback chains.

Use it when:

- different workloads should go to different agents
- you want routing by issue state or profile
- you are comparing providers and want failover built in

## `pipeline-static`

Fixed-stage pipeline with research, coding, and review.

Use it when:

- every issue should follow the same sequence
- you want clearer handoffs between stages
- you prefer deterministic orchestration over planner-driven decomposition

Note:

- this starter enables automation, so it still needs real tracker wiring before validation fully
  passes

## `pipeline-planner`

Planner-driven pipeline that creates a structured plan before execution.

Use it when:

- issues vary widely in scope and shape
- you want a planner to decide task breakdown
- you are comfortable with a little more orchestration complexity for better decomposition

Note:

- this starter enables automation, so it still needs real tracker wiring before validation fully
  passes

## `automation-feedback`

Automation-oriented workflow with draft PRs and human feedback channels.

Use it when:

- the main goal is handoff automation
- you want notifications and external feedback loops early
- your repo already expects human review after automated work

Note:

- this starter enables automation, so it still needs real tracker wiring before validation fully
  passes
