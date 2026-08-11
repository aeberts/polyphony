# Historical Beads reference

> **Historical reference only.** This directory is not active configuration and
> is not an issue-tracking source for the Polyphony Safety Fork. Do not copy it
> to a root `.beads` directory.

The original project included a root `.beads` directory that configured a
Dolt-backed Beads tracker. The Safety Fork uses
[GitHub Issues for `aeberts/polyphony`](https://github.com/aeberts/polyphony/issues)
and the [Polyphony safety fork GitHub Project (#6)](https://github.com/users/aeberts/projects/6)
for delivery work instead.

The archived files preserve the useful non-runtime context from that original
directory:

- [`inherited-readme.md`](inherited-readme.md) describes the original Beads
  workflow and commands.
- [`inherited-config.yaml`](inherited-config.yaml) records its repository-local
  configuration template.
- [`inherited-metadata.json`](inherited-metadata.json) records the original
  Dolt backend metadata.

The root directory and its hooks were removed because a present `.beads`
directory is discovered by Polyphony as a supplemental runtime tracker. Beads
product support remains in the codebase.
