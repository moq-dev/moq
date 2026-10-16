# [S] Narrow the model before it goes public

## Goal

Every model-internal Rust item in `rs/moq-net/src/model/` is
`pub(in crate::model)` (or `pub(super)`), so a `pub(crate)` left there is
either marked deliberately private (passthrough, tuning, capability gap,
test-only) or marked pending with the
[public model](/quest/m1/public-model/README.md) child that owns it. A new
model item a session reaches then fails to compile until someone makes it
public or marks it, instead of silently growing the private surface while
the other children are in flight.

## Plan

Run this before the other children so their work only shrinks the marked
set. Use the epic's inventory method: narrow every `pub(crate)` in
`model/`, then restore each item `cargo check -p moq-net --features fuzz`
(and `--tests`) names, adding a one-line comment that says which child
owns it or why it stays private.

Enforce the marks in CI, or they drift. Narrowing makes the compiler
reject unmarked reach, but a contributor can still restore `pub(crate)`
without a comment. Add a check to the lint that `just check` and CI already
run: every restricted visibility under `rs/moq-net/src/model/` broader than
`pub(in crate::model)` (any `pub(crate)` or `pub(in crate)` spelling, and a
`pub(super)` in `model/mod.rs`, whose parent is the crate root) must carry a
marker
comment from a fixed vocabulary (for example `// private: passthrough`,
`tuning`, `capability gap`, `test`, or `// pending: <quest path>`). A
`pending` mark must name a quest that still exists. Existence is enough
because a finished quest is deleted in the PR that completes it (`quest
guide`: "Finished quests are deleted"). The check is a grep. It
pairs with the narrowed visibility, so it does not need the compile probe.
Each child's PR then deletes its `pending` marks as items go public, and the
check fails if one is left behind.

No public API or wire change.
