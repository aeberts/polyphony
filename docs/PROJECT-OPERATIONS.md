# Project operations

## Archive decision

This project concluded on 2026-08-13. The Polyphony Safety Fork and the earlier
Symphony trial are retained for historical reference and are not active work
systems.

Live testing showed that Polyphony's deterministic gates, isolated workspaces,
durable state, and fail-closed behavior were useful, but the system did not
provide the judgment needed at natural-language handoffs. Small representation
differences could stop a valid implementation, require operator diagnosis, or
make an automatic retry unsafe. A coordinating agent was still required to
interpret intent, reconcile evidence, and choose a safe recovery.

The adoption decision is **reject as a primary delivery coordinator**. Use the
judgment-led closed-loop issue-delivery skill instead. Preserve this repository,
GitHub Issues, Project #6, and trial artifacts as evidence. Do not start new
Polyphony work or live tests unless a later explicit decision reactivates the
experiment.

## Historical purpose and authority

This repository, [`aeberts/polyphony`](https://github.com/aeberts/polyphony),
is the independently maintained Polyphony Safety Fork. It is not maintained as
a branch intended for routine merging into the original upstream project.

The historical delivery tracker was:

- [GitHub Issues for `aeberts/polyphony`](https://github.com/aeberts/polyphony/issues)
- [Polyphony safety fork GitHub Project (#6)](https://github.com/users/aeberts/projects/6)

The issues remain the durable record of scope, decisions, implementation
evidence, QA findings, and integration status. Project #6 shows the terminal
state of the experiment.

[`aeberts/symphony-trial`](https://github.com/aeberts/symphony-trial) and its
[GitHub Project (#5)](https://github.com/users/aeberts/projects/5/views/3) are
used only to test this fork with the earlier orchestration system. They do not
track delivery work for this repository.

[`LIVE-TESTING.md`](../LIVE-TESTING.md) preserves the procedure used for
preparation, isolation, launch, observation, recovery, and evidence. It is not
authorization to start another coordinator or live attempt.

## Historical session procedure

The following procedure documents how work was started before archival. It is
retained for context and is not authorization to resume an issue.

For a request such as "work on issue #26":

1. Read [`AGENTS.md`](../AGENTS.md) and this document.
2. Read issue `#26` in `aeberts/polyphony`, including comments and linked pull
   requests.
3. Inspect the issue's item and status in Project #6.
4. Confirm the checked-out branch and current Git state before editing.
5. Keep the work limited to the issue's acceptance criteria. Put durable
   progress and verification evidence on the issue.

If the issue, Project #6, and repository state disagree, do not silently pick
one. Record the conflict on the issue and request direction.

## Historical delivery workflow

1. Refine acceptance criteria when they cannot distinguish a real fix from a
   happy-path result.
2. Create or use an issue-scoped branch from the current approved integration
   branch. Use the `codex/` prefix unless the issue specifies another name.
3. Implement only the selected issue. Run focused validation, then the
   repository checks required for the changed code.
4. Commit with the conventional-commit format documented in `AGENTS.md`.
5. Add an implementation note to the GitHub issue: commit, changed behavior,
   checks, acceptance-criterion mapping, unverified paths, and QA status.
6. Obtain independent QA when the issue workflow requires it. QA must record a
   clear PASS or FAIL on the issue, with evidence for every acceptance
   criterion.
7. Integrate only after the required human or issue-workflow approval. Record
   the target branch, commit or pull request, checks, and remaining limits on
   the issue.

Handoffs are deltas, not a second operating manual. Include only the issue and
branch, completed work, checks run, open risk or blocker, and the exact next
action. Put recurring process information in this document instead.

## Historical upstream relationship

The fork is **selectively upstreamable**. Build what the safety fork needs.
Evaluate upstream compatibility only when a change has likely shared value.
When preparing a potentially upstreamable change, isolate it where practical,
document intentional divergence, and prepare a focused contribution. Do not
constrain routine fork work around hypothetical upstream adoption.

## Inherited Beads context

The repository includes Beads product support. Historical Beads tracker
material is archived in [`docs/reference/beads-history/`](reference/beads-history/)
to explain the original project's tracker model. It is not the active issue
store, delivery tracker, or configuration source for the Polyphony Safety Fork.
The root `.beads` directory is deliberately absent so Polyphony cannot activate
it as a supplemental runtime tracker. Do not remove Beads product support as
part of normal issue work.

## Historical repository configuration

`polyphony.toml` configured this repository's Polyphony runtime for GitHub
Issues in `aeberts/polyphony` and Project #6. It must not be treated as a
replacement for the issue record. Keep credentials outside the repository and
use environment variables or personal configuration for them.
