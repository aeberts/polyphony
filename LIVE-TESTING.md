# Live testing Polyphony

> **Historical procedure:** The Polyphony experiment concluded on 2026-08-13
> and this repository is no longer used for live orchestration. Preserve this
> guide and its referenced artifacts as evidence. Do not launch a coordinator,
> dispatch an issue, reuse a runtime, or call a model from this procedure unless
> a new explicit human decision reactivates the experiment.

## Purpose

Use this guide to run a bounded live Polyphony test against a separate,
disposable repository. Live tests confirm behavior that unit and integration
fixtures cannot fully prove, such as a real Codex app-server session, GitHub
Project transitions, process shutdown, system-owned local commits, and durable
tracker evidence.

Do not run a live test against this development repository. For the current
environment:

- `aeberts/polyphony` and GitHub Project #6 are the development tracker.
- `aeberts/symphony-trial` and GitHub Project #5 are test-only.
- `/Users/zand/dev/polyphony-safety-fork` supplies the Polyphony binary.
- `/Users/zand/dev/symphony-trial` supplies the test configuration, runtime
  database, workspaces, logs, and disposable target repository.

Replace these example paths and tracker identifiers when a different test
repository is used.

## Authority and evidence

Keep development and test evidence separate:

- Record implementation, automated QA, integration, and the reason for a live
  test on the relevant `aeberts/polyphony` issue.
- Create a fresh issue in the test repository for each new live scenario or
  materially different attempt. Add it to the test project.
- Put the live issue in the configured eligible state, currently `Ready`, only
  when its scope and acceptance criteria are ready to dispatch.
- Record live implementation, QA, recovery, and final PASS or FAIL evidence on
  the test issue.
- When a test concludes, set only that issue's project item to `Done` and close
  the issue. A failed test can be done: state clearly that it concluded without
  a validation PASS and link the development defect. Do not mark the whole test
  project done while testing continues.

Preserve old issue comments, databases, workspaces, and logs as historical
evidence. Do not rewrite a failed test into a new scenario. Create a successor
issue and link the records.

## Safety boundary

Every live test must state what it may change. The normal boundary is:

- one explicitly selected test issue;
- one disposable workspace created from the test repository;
- one fresh runtime database, unless the test explicitly exercises restart or
  recovery;
- local repository changes and a local-only Polyphony-owned commit when the
  scenario enables `local_commit`;
- durable comments and workflow-state changes on the selected test issue.

Unless the issue explicitly authorizes more, prohibit pushes, pull requests,
merges, deployments, releases, production repositories, unrelated issues, and
changes outside the test repository.

Do not deliberately call a real model to test a failure that a fake process or
disposable RuntimeService fixture can prove. A live issue must identify the
specific boundary that requires a real provider session.

## Create a dispatchable test issue

Write acceptance criteria that distinguish a real success from a partial or
happy-path result. For a closed-loop handoff test, require direct evidence for
all relevant ordering and failure gates:

1. The configured agent starts successfully.
2. The worker makes only the bounded requested change.
3. The worker records complete, role-matched durable evidence.
4. Polyphony confirms worker shutdown before downstream work.
5. When enabled, the system-owned local commit is durable before QA starts.
6. Independent QA is read-only and records an explicit PASS or FAIL.
7. Repair starts only after a genuine implementation or QA failure and the
   configured tracker transition succeeds.
8. No prohibited external action occurs.

Use a new marker, file, or behavior for a successor test. Reusing an artifact
from an earlier workspace makes duplicate-work and clean-checkout evidence
ambiguous.

## Prepare an isolated run

Use three identifiers for every fresh attempt:

- test issue number, for example `30`;
- run ID, for example `issue-30-post32-20260813-r1`;
- workspace root, for example
  `.polyphony/issue-30-post32-20260813-r1-workspaces`.

The run ID names the SQLite database. The workspace root belongs in the
live-test workflow. Both must be new for a fresh attempt.

Do not reuse a database or workspace merely because an earlier attempt failed.
Reuse both only for an intentional restart or recovery test whose acceptance
criteria require preservation of the earlier state. Record that decision on
the test issue before restarting.

### Reference tracker and pipeline configuration

Keep credentials outside both repositories. The test repository's local
configuration should select its own repository and project, not Project #6:

```toml
[tracker]
kind = "github"
repository = "aeberts/symphony-trial"
api_key = "$GITHUB_TOKEN"
project_owner = "aeberts"
project_number = 5
project_status_field = "Status"
active_states = ["Ready", "In progress", "Repair Needed", "Needs Human Decision"]
terminal_states = ["Done"]
stop_when_ineligible = true

[orchestration]
dispatch_mode = "manual"
```

Use a dedicated workflow for the live scenario. The following sections show
the safety-critical shape; prompts and stages may be narrowed for the issue:

```yaml
---
tracker:
  kind: github
  active_states: [Ready, In progress, Repair Needed, Needs Human Decision]
  terminal_states: [Done]
  blocked_state: Needs Human Decision
  stop_when_ineligible: true
polling:
  interval_ms: 30000
workspace:
  root: .polyphony/issue-30-post32-20260813-r1-workspaces
  checkout_kind: discrete_clone
  source_repo_path: /Users/zand/dev/symphony-trial
  sync_on_reuse: true
agent:
  max_concurrent_agents: 1
  max_turns: 8
  max_retry_backoff_ms: 60000
tools:
  enabled: true
  allow:
    - workspace_list_files
    - workspace_read_file
    - workspace_search
    - issue_comment
pipeline:
  enabled: true
  stages:
    - { category: coding, role: implementation, agent: live-issue30-implementer }
    - { category: review, role: qa, agent: live-issue30-qa }
    - { category: coding, role: repair, agent: live-issue30-repair }
    - { category: review, role: qa, agent: live-issue30-qa }
local_commit:
  enabled: true
agents:
  default: live-issue30-implementer
  profiles:
    live-issue30-implementer:
      kind: codex
      transport: app_server
      command: codex app-server
      approval_policy: auto
      thread_sandbox: workspace-write
      turn_sandbox_policy: workspace-write
    live-issue30-qa:
      kind: codex
      transport: app_server
      command: codex app-server
      approval_policy: always
      thread_sandbox: read-only
      turn_sandbox_policy: read-only
    live-issue30-repair:
      kind: codex
      transport: app_server
      command: codex app-server
      approval_policy: auto
      thread_sandbox: workspace-write
      turn_sandbox_policy: workspace-write
---
```

The legacy `auto` and `always` values in this example intentionally exercise
Polyphony's Codex protocol translation. Use only values relevant to the test.
Use run-specific profile names. Configuration merges with user-level profiles,
so generic names such as `implementer` can inherit an unrelated command,
model, approval policy, or sandbox setting.

With `local_commit.enabled = true`, implementation and repair must use
`workspace-write` for both sandbox settings, and QA must use `read-only` for
both. Polyphony rejects other sandbox combinations for this gate.

The workflow prompt must also say:

- implementation and repair edit and validate only;
- agents must not run `git add`, `git commit`, push, or other Git mutations;
- a denied Git-metadata write is not a blocker because Polyphony owns the
  local-commit gate;
- QA must not edit or commit;
- every role must publish the evidence required by the pipeline;
- no automation action is authorized.

## Preflight

Complete all checks before starting the interface.

### 1. Verify trackers and issue state

- Confirm the development issue is integrated into the intended Polyphony
  branch and records any required live-test gate.
- Read the live issue and all comments.
- Confirm the live issue belongs to the test repository and Project #5.
- Confirm the selected item is `Ready`. Other ready test items can remain in
  Project #5, but do not dispatch them as part of this run.
- Confirm the issue task is compatible with the current test repository tip.

### 2. Build the exact Polyphony candidate

From `/Users/zand/dev/polyphony-safety-fork`:

```bash
git status --short --branch
git fetch origin --prune
git rev-parse HEAD origin/main
just format-check
just lint
cargo +nightly-2025-11-30 build --locked -p polyphony-cli
```

The worktree must be clean, and `HEAD` must be the approved commit intended for
the test. Do not trust an existing `target/debug/polyphony`; it can predate the
checked-out source. Record the Polyphony commit SHA and binary path on the test
issue or in the run notes.

Run the focused automated tests required by the development issue before
spending a live model call. A prior CI or QA PASS is not a substitute when the
local binary was built from a different commit.

### 3. Check the test repository

From `/Users/zand/dev/symphony-trial`:

```bash
git status --short --branch
git fetch origin --prune
git rev-parse HEAD origin/main
```

Identify every local change. Local workflow files, fixtures, launch scripts,
and `.polyphony/` artifacts can be intentional, but they must not be mistaken
for the issue's requested change or enter the system-owned commit. The
disposable clone must start from the intended clean source revision.

### 4. Validate configuration without starting a coordinator

Use the exact binary and workflow planned for the run:

```bash
/Users/zand/dev/polyphony-safety-fork/target/debug/polyphony \
  -C /Users/zand/dev/symphony-trial \
  --workflow POLYPHONY-LIVE-ISSUE-30.md \
  config

/Users/zand/dev/polyphony-safety-fork/target/debug/polyphony \
  -C /Users/zand/dev/symphony-trial \
  --workflow POLYPHONY-LIVE-ISSUE-30.md \
  doctor
```

Inspect the merged configuration. Confirm the test repository, Project #5,
active and terminal states, manual mode, unique workspace root, exact stage
order, role profiles, sandboxes, tool allowlist, local-commit setting, and lack
of push/PR/merge/deploy automation. For every profile referenced by the
pipeline, confirm the effective command, transport, model, approval policy,
and both sandbox values. Stop if `config --json` or `doctor` shows an inherited
value that the live workflow did not authorize, especially a dangerous bypass
command. Unreferenced user-level profiles can appear in the merged output; the
pipeline must not select them.

### 5. Verify the launcher for the selected live issue

Inspect the exact launch script or saved launch command that the operator will
use. Do not assume that a launcher from an earlier attempt is still correct.
Before approval, verify that it identifies the current live-test issue and
matches the configuration validated in the previous step:

- exact Polyphony candidate binary path;
- current issue-specific workflow file;
- fresh run ID and its exact SQLite path;
- current workflow's unique workspace root;
- test repository working directory;
- manual dispatch mode from the effective merged configuration;
- structured JSON logging for coordinator observation;
- the intended interface and port, when using the web interface; and
- no push, pull-request, merge, deploy, release, or other automation.

For a fresh attempt, the launcher must stop if either the SQLite file or
workspace root already exists. It must not silently default to an earlier
issue, workflow, database, or workspace. Do not include or use an environment
override that permits runtime reuse. Reuse is allowed only for an intentional
recovery test whose recorded scope requires the same runtime.

Run a syntax check without executing the launcher, for example:

```bash
bash -n /Users/zand/dev/symphony-trial/run-polyphony-closed-loop.sh
```

Then compare the launcher's resolved values with the recorded preflight and
the effective `config --json` output. If any value differs, correct the
launcher and repeat configuration and isolation checks before requesting
approval. Do not start the launcher as part of this check.

### 6. Prove runtime isolation

Before launch:

- Confirm no Polyphony coordinator is already running for the test repository.
- Confirm the proposed SQLite file does not exist for a fresh attempt.
- Confirm the proposed workspace root does not exist for a fresh attempt.
- Confirm the port is unused when starting the web interface.
- Stop or disregard stale log-tail processes that point at older runs; they do
  not observe the new log.

Never set an “allow reuse” option to get past an existing database during a
fresh test. Choose a new run ID. For an intentional recovery test, use the same
binary version, database, workflow, and workspace unless the test explicitly
measures an upgrade boundary.

## Launch and dispatch

Export a descriptive run ID and construct its database path explicitly:

```bash
export POLYPHONY_LIVE_RUN="issue-30-post32-20260813-r1"
export POLYPHONY_LIVE_DB="/Users/zand/dev/symphony-trial/.polyphony/runtime/${POLYPHONY_LIVE_RUN}.sqlite"
export GITHUB_TOKEN="$(gh auth token)"

/Users/zand/dev/polyphony-safety-fork/target/debug/polyphony \
  -C /Users/zand/dev/symphony-trial \
  --workflow POLYPHONY-LIVE-ISSUE-30.md \
  --sqlite-url "sqlite://${POLYPHONY_LIVE_DB}?mode=rwc" \
  --log-json
```

For the HTTP interface, insert `serve --address 127.0.0.1 --port 8080` after
the binary name and keep the same directory, workflow, SQLite URL, and JSON log
options.

Start only one coordinator for a runtime database. Keep dispatch mode manual.
Verify that the interface shows the expected repository and selected issue,
then manually dispatch that issue once. Do not switch to automatic mode for a
single-issue validation.

## Observe without changing the run

Use the running TUI or web interface, its structured JSON log, the provisioned
workspace's role logs, process inspection, and GitHub issue comments.

The coordinator writes a new file under the test repository's
`.polyphony/logs/` directory. Select the file created by the current process,
record its exact path, and follow that path directly:

```bash
tail -F /Users/zand/dev/symphony-trial/.polyphony/logs/polyphony-<timestamp>-<nanoseconds>-pid<pid>.jsonl \
  | jq -r 'select(.timestamp and .level and .fields.message) | "\(.timestamp) [\(.level)] \(.fields.message)"'
```

Each dispatched app-server agent also writes role-specific artifacts inside
the dynamically provisioned workspace's `.polyphony/` directory. These are the
best source for the errors, tool calls, and agent reasoning visible within one
implementation, QA, or repair session. Typical files are:

```text
<workspace>/.polyphony/implementer-appserver.log
<workspace>/.polyphony/implementer-appserver.cast
<workspace>/.polyphony/implementer-prompt.md
<workspace>/.polyphony/qa-appserver.log
<workspace>/.polyphony/repair-appserver.log
```

The role files appear only after that role starts. Do not assume that the
workspace is named `_26` or any other fixed value. Start from the unique
workspace root recorded during preflight and discover the selected issue's
workspace and agent artifacts:

```bash
find /Users/zand/dev/symphony-trial/.polyphony/issue-30-post32-20260813-r1-workspaces \
  -type f \( -name '*-appserver.log' -o -name '*-appserver.cast' -o -name '*-prompt.md' \) \
  -print
```

Record the exact absolute path as soon as a role starts. Follow the readable
role log to see the agent's messages and provider/tool errors in real time:

```bash
tail -F /absolute/path/to/the/provisioned/workspace/.polyphony/implementer-appserver.log
```

Use the corresponding `qa-appserver.log` or `repair-appserver.log` after those
roles dispatch. The `.cast` file preserves the fuller app-server transcript,
and the prompt file proves the instructions supplied to that role. When
reporting a finding from Codex, cite the role-log path and timestamp as well as
the outer coordinator log. In a local handoff that supports file links, use an
absolute link such as
`[implementer app-server log](/absolute/workspace/.polyphony/implementer-appserver.log)`.

Useful read-only checks include:

```bash
ps -axo pid,ppid,lstart,state,command | rg '[p]olyphony|[c]odex app-server'
git -C /absolute/path/to/the/provisioned/workspace status --short --branch
gh issue view 30 --repo aeberts/symphony-trial --comments
```

Do not run `polyphony data` against a database owned by a live TUI, server, or
daemon. The command currently starts another RuntimeService and can reconcile
or fail live work; this limitation is tracked in `aeberts/polyphony#30`.
Likewise, do not start a second TUI/server/daemon, run `polyphony reset`, open
the live database with a writer, or reuse the database from another process.

Watch for the scenario's exact lifecycle events. For a normal closed-loop
handoff, capture evidence of:

```text
implementation start
→ durable implementation completion evidence
→ app-server stop begins
→ app-server stop confirmed
→ durable system local-commit outcome
→ independent QA start
→ durable QA PASS or FAIL evidence
```

A shutdown timeout or typed stop failure after durable completion is a
lifecycle handoff block. It must preserve completion evidence, dispatch no
repair, and start neither local commit nor QA until an operator confirms that
no owned worker remains and explicitly invokes the supported recovery action.
Do not describe this state as an implementation failure.

## Stop conditions

Stop dispatch and preserve evidence when any of these occurs:

- the repository, project, issue, workflow, binary SHA, database, or workspace
  differs from the recorded preflight;
- a second coordinator may own or have opened the runtime database;
- an agent attempts a prohibited Git or external action;
- durable evidence cannot be written or validated;
- worker shutdown is not confirmed;
- local commit or QA starts out of order;
- repair starts for a lifecycle handoff block;
- tracker and runtime state disagree;
- the run reaches `Needs Human Decision` or another explicit recovery gate.

Do not clean up, reset, retry, or dispatch again until the failure window is
understood and the evidence paths are recorded.

## Recovery versus a new attempt

Use the existing runtime only when testing or performing supported recovery.
Before recovery:

1. Confirm from process state that no owned agent session remains.
2. Record the runtime database, workspace, log, run/task status, issue comments,
   and observed failure on the test issue.
3. Keep the same issue, database, workspace, workflow, and binary for a pure
   restart test.
4. Use the interface's explicit retry/recovery action once.
5. Verify that completed implementation is not duplicated and downstream work
   remains gated until recovery.

If the goal is to validate a code fix or repeat the scenario from a clean
boundary, create a new test issue when the old issue is already a historical
record, and always use a new database, workspace root, and run ID.

## Conclude and preserve the test

For PASS, add a final test note with:

- Polyphony source commit and binary path;
- test repository commit used as the clone source;
- issue and Project #5 state transitions;
- runtime database, workspace, outer log, and relevant role-log paths;
- direct absolute links to each dispatched role's app-server log and prompt;
- the observed event ordering with timestamps;
- local-commit SHA and changed files, when enabled;
- durable implementation and QA comment links;
- checks run and results;
- prohibited actions confirmed absent;
- any unverified path or operational limit.

For FAIL, record the same identifiers plus the first incorrect event, expected
behavior, actual behavior, preserved diagnostics, and the linked development
issue. Do not call a bounded or informative failure a PASS.

Set the individual test item to `Done` and close it only after the conclusion
note is durable. “Done” means the attempt is terminal and preserved; the note
must say whether validation passed or failed. Keep Project #5 active for later
tests.

Do not delete the runtime database, workspaces, logs, or historical issue
comments as part of normal conclusion. If storage cleanup becomes necessary,
first copy or summarize the evidence required to reproduce the finding and
obtain explicit approval for the deletion scope.
