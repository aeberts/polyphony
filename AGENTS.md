# Repository guidance

This repository is the independently maintained Polyphony Safety Fork. Read
[`docs/PROJECT-OPERATIONS.md`](docs/PROJECT-OPERATIONS.md) before starting work.
It defines the current work tracker, issue workflow, validation expectations,
and handoff format.

## Start work from an issue

When asked to work on issue `#<number>`, treat it as
[`aeberts/polyphony` Issues](https://github.com/aeberts/polyphony/issues). Read
the issue, its comments, and its item in the
[Polyphony safety fork GitHub Project (#6)](https://github.com/users/aeberts/projects/6)
before changing code. Use that issue and Project #6 for delivery status and
durable evidence.

[`aeberts/symphony-trial`](https://github.com/aeberts/symphony-trial) and its
[GitHub Project (#5)](https://github.com/users/aeberts/projects/5/views/3) are
test-only. They are not this repository's delivery tracker.

## Working policy

- This fork is independently maintained. Changes are selectively upstreamable:
  assess upstream compatibility when a change has shared value; do not make it
  a default constraint.
- Beads remains part of the Polyphony product and inherited repository context,
  but it is not the operational issue tracker for this fork.
- Keep work scoped to the selected issue. Record implementation, QA, and any
  integration evidence on that GitHub issue.
- Use conventional commits:
  `feat|fix|docs|style|refactor|test|chore(scope): description`.
- Before committing code, run `just format` and `just lint`. For documentation
  changes, run focused checks that prove links, file state, and stale guidance
  are correct.

## Engineering conventions

- Do not use `unwrap()` or `expect()` in non-test Rust code. Use typed,
  explicit error handling.
- Add focused tests for behavior changes. Bug fixes need a regression test.
- Keep implementation out of `main.rs`, `lib.rs`, and `mod.rs`; use named,
  focused modules instead.
- Centralize third-party dependency versions in the root `Cargo.toml` under
  `[workspace.dependencies]`.
- Keep tracker requests batched and cached where practical. Do not shell out to
  external CLIs from Rust when a Rust API client is available.
