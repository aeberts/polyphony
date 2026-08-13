# Repository guidance

This repository is an archived historical record of the independently
maintained Polyphony Safety Fork. Do not start new implementation, live tests,
coordinators, or app-server workers. Preserve its code, issues, databases,
workspaces, logs, and documentation for reference.

Read [`docs/PROJECT-OPERATIONS.md`](docs/PROJECT-OPERATIONS.md) for the final
decision and historical operating model.

## Historical issue records

Historical issue `#<number>` references mean
[`aeberts/polyphony` Issues](https://github.com/aeberts/polyphony/issues). Read
the issue, its comments, and its item in the
[Polyphony safety fork GitHub Project (#6)](https://github.com/users/aeberts/projects/6)
as preserved evidence. Do not reopen or resume work without a new explicit
human decision to reactivate the experiment.

[`aeberts/symphony-trial`](https://github.com/aeberts/symphony-trial) and its
[GitHub Project (#5)](https://github.com/users/aeberts/projects/5/views/3) are
test-only. They are not this repository's delivery tracker.

## Historical live testing

[`LIVE-TESTING.md`](LIVE-TESTING.md) is retained as the procedure used for the
trial. Do not use it to launch a new test unless the experiment is explicitly
reactivated. Never open a preserved runtime database with a coordinator or
observer RuntimeService.

## Preservation policy

- Do not delete or rewrite historical issue comments, runtime databases,
  workspaces, logs, or live-test artifacts.
- Do not use Polyphony or Symphony for production or development work.
- The active delivery method is the judgment-led closed-loop issue-delivery
  skill with durable tracker evidence and explicit human integration gates.
- Any future evaluation must start with a new decision record, new issue, and
  new isolated runtime. Historical runtimes are read-only evidence.

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
