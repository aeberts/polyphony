# Beads - inherited tracker notes

> **Historical reference only.** This is a copy of the useful guidance from the
> original root `.beads/README.md`. It is not active configuration and must not
> be used to create, list, or update Safety Fork delivery work.

The original project described Beads as a Git-native, Dolt-backed, CLI-first
issue tracker. Its usual commands were:

```sh
bd create "Add user authentication"
bd list
bd show <issue-id>
bd update <issue-id> --claim
bd update <issue-id> --status done
bd dolt push
```

Beads was presented as branch-aware and as synchronizing tracker state with
Git. This explains the inherited configuration, but the Safety Fork's
authoritative workflow is GitHub Issues plus Project #6.
