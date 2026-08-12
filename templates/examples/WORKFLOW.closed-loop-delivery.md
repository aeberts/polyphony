---
# Destination: copy to WORKFLOW.md in the repository root.
# Seed a real tracker during `polyphony init`: evidence notes are tracker comments.
tracker:
  kind: none
polling:
  interval_ms: 60000
workspace:
  root: .polyphony/workspaces
  # Each worker gets an isolated repository checkout and its own Git metadata.
  checkout_kind: discrete_clone
  sync_on_reuse: true
agent:
  # Eligible issues can run concurrently, while each issue remains sequential.
  max_concurrent_agents: 2
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
    - { category: coding, role: implementation, agent: implementer }
    - { category: review, role: qa, agent: qa }
    - { category: coding, role: repair, agent: repair }
    - { category: review, role: qa, agent: qa }
    - { category: coding, role: repair, agent: repair }
    - { category: review, role: qa, agent: qa }
agents:
  default: implementer
  profiles:
    implementer:
      kind: codex
      transport: app_server
      command: codex app-server
      approval_policy: auto
      thread_sandbox: workspace-write
      turn_sandbox_policy: workspace-write
    qa:
      kind: codex
      transport: app_server
      command: codex app-server
      approval_policy: always
      thread_sandbox: read-only
      turn_sandbox_policy: read-only
    repair:
      kind: codex
      transport: app_server
      command: codex app-server
      approval_policy: auto
      thread_sandbox: workspace-write
      turn_sandbox_policy: workspace-write
---
# Bounded Closed-Loop Delivery

Run only issues that have explicit tracker approval. The workflow is:

`implementation → independent QA → repair → fresh QA → repair → fresh QA`.

- QA must inspect and test only. It cannot change workspace files, commit, push, create pull
  requests, or dispatch repairs.
- Every implementation, QA, and repair stage must publish the required tracker evidence note.
- Only `QA PASS` completes the issue. A QA failure moves only that issue to the next repair.
- After two completed repairs, a further QA failure is recorded as **Needs Human Decision** and
  no worker is dispatched. A human must decide any later work.
- This pack intentionally enables no repository automation, merge, deployment, or release action.
