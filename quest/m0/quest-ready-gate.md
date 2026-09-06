# [S] quest: nothing reports whether a quest is startable

## Goal

`quest ready` answers "is this quest blocked, and by what" from the tree, so the
start and spawn flows stop reconstructing that answer by grepping.

## Plan

[quest/AGENTS.md](/quest/AGENTS.md) says only ready quests are executed and that
a missing `Required` section is what makes one ready, but the only thing that
reads `Required` is `quest check`, which proves the section is well-formed and
acyclic. Those are properties of the tree; neither says whether a given quest can
be started now.

The gap has been paid.
[#2296](/quest/m1/2296-moq-native-bring-the-quiche-backend-to-quinn-noq-feature.md)
was started, and PR #3418 landed a large piece of it, while its `Required` bullet
on the noq parity gate still stood. The bullet was dropped in that PR after the
fact, which was the right call and not one anything had asked for.

Add a `ready` subcommand to `rs/quest`:

- with a path, print the blocker chain: each `Required` entry, and for a required
  questline, which of its quests are still open. A plain-text bullet is always a
  blocker, since nothing in the tree can clear it;
- with no path, list every ready quest in tree order, which is the query the
  start flow reproduces by grepping today;
- exit 0 either way. This is advice, not a gate: a caller under `set -e` would
  otherwise turn a blocked quest into a hard stop, and starting a blocked quest
  deliberately is sometimes right. Reserve a non-zero exit for the command
  failing, the way any other tool does.

Liveness stays out. A quest can be ready, coherent, and already done:
[#2979](/quest/m1/2979-moq-tokio-does-not-compile-with-no-default-features-and.md)
was recommended on the strength of its plan when PR #3150 had closed the
underlying issue in August. Checking that means asking GitHub, which is the
calling flow's job, not a tree validator's; `quest ready` stays offline and
deterministic.

Note in the failure text that a blocked quest with an independently landable
piece is split into its own quest, per the Creation rule, rather than started as
it stands. Document the subcommand in `quest/AGENTS.md` beside `quest check`.
