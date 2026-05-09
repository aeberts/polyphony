# Paperclip-Informed Roadmap

This roadmap translates the Paperclip comparison into Polyphony-native work.

The goal is not to copy Paperclip's company metaphor, org charts, or multi-tenant control plane.
The goal is to copy the parts that reduce friction and improve operator trust while staying aligned
with Polyphony's repo-native architecture.

## Principles

- Keep `WORKFLOW.md` as the source of truth.
- Stay single-repo and workflow-centric.
- Prefer stronger onboarding, summaries, and artifacts over extra abstraction.
- Treat plugins, multi-tenant control planes, and board/governance metaphors as out of scope for
  now.

## Phase 1, Onboarding

### 1. Explicit bootstrap command

Status: completed

- Add `polyphony init` as the first-class entry point for bootstrapping a repo.
- Support starter templates so users can begin from a concrete workflow shape instead of editing a
  giant reference file from scratch.
- Auto-detect local agents and repo tracker wiring where possible.

Why:

- Polyphony already had bootstrap behavior, but it was implicit.
- Paperclip wins hard on "I can get to first success in one command."

### 2. Sharper starter templates

Status: completed

- Curate a small set of opinionated starter templates:
  - single-agent codex
  - multi-agent routing
  - static pipeline
  - planner pipeline
  - automation plus feedback
- Document when each template is the right default.

Why:

- The current examples exist, but they are discoverable only if users browse the repo.

## Phase 2, Operator Clarity

### 3. Progressive disclosure in TUI and web

Status: completed

- Add synthesized run summaries ahead of raw logs.
- Show outputs and artifacts first: branch, commit, PR, changed files, test results, review
  outcome, handoff state.
- Keep raw event/log streams one level deeper for debugging.

Why:

- Paperclip is correct that operators should see "what happened" before they see terminal exhaust.

### 4. Better historical run views

Status: completed

- Improve persisted run history in SQLite and the web UI.
- Make it easy to answer:
  - what ran
  - why it stopped
  - what changed
  - what it cost
  - what needs human action

Why:

- Polyphony is already strong at live orchestration. History and handoff comprehension should catch
  up.

## Phase 3, Workflow Packs

### 5. Reusable workflow packs

Status: completed

- Promote starter workflows from static examples into named packs that `polyphony init` can select
  directly.
- Consider light parameterization for common setup values.

Why:

- This captures the useful part of Paperclip's "templates" idea without importing its company model.

### 6. Guided tracker and auth setup

Status: completed

- Extend `polyphony init` to detect common tracker layouts and point users at the exact missing
  credential or config field.
- Make `polyphony doctor` the obvious next step from init output.

Why:

- Activation energy is still too high when the repo is valid but the credentials are not.

## Explicit Non-Goals

- Company or board abstractions
- Org charts and reporting lines
- Multi-company tenancy
- DB-first config editing
- Plugin marketplace or dynamic extension runtime

Those are Paperclip-native ideas. Polyphony should stay repo-native and execution-centric.
