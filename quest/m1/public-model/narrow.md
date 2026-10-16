# [S] Narrow the model before it goes public

## Goal

Every model-internal Rust item in `rs/moq-net/src/model/` is
`pub(in crate::model)` (or `pub(super)` outside `model/mod.rs`), so a `pub(crate)` left there is
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
run. It is an allowlist, not a list of bad spellings (`pub(in
super::super)` from `model/group.rs` reaches `crate` too). Under
`rs/moq-net/src/model/`, every restricted visibility fails without a marker
unless it is `pub(self)`, `pub(super)` outside `model/mod.rs`, or a
`pub(in ...)` path starting with `crate::model`. A marker is a
comment from a fixed vocabulary (for example `// private: passthrough`,
`tuning`, `capability gap`, `test`, or `// pending: <quest path>`). A
`pending` mark must name a quest that still exists. Existence is enough
because a finished quest is deleted in the PR that completes it (`quest
guide`: "Finished quests are deleted"). The check is a grep, recommended
over reading effective visibility from rustdoc JSON, which needs nightly
and an unstable format for the same answer. It
pairs with the narrowed visibility, so it does not need the compile probe.
Each child's PR then deletes its `pending` marks as items go public, and the
check fails if one is left behind.

JS needs the same guard, since nothing compiles reach there. Custom
transports cannot see this surface for two different reasons:
`internal.ts` and `wire.ts` are not exported from `src/index.ts`, and
`stripInternal` drops `@internal` members of exported classes from the
declarations. Neither needs a new `@internal` tag. The classification
marker is a separate comment from the shared vocabulary above, not the
`@internal` tag. Add it to: every export of `js/net/src/internal.ts`
and `wire.ts` (each `hooks` entry and view member included), and every
`@internal` member in the model modules, plus every declaration session
code already imports from a module the index does not export (for example
`lite/subscriber.ts` reaching `BroadcastCache` in `consume.ts`, `Tail` in
`tail.ts`, and `sendOrder` in `priority.ts`), so the check passes on its
first run. Seed them by running the grep below and marking each hit. Then
add an import-edge grep to the
same lint. It fails when a non-test file under `src/{ietf,lite,connection}/`
imports a name from `internal.ts`, `wire.ts`, or a module the index does not
export, or reads a `hooks.<name>` or view member, whose declaration has no
marker. Recommended over a custom Biome (GritQL) or ESLint rule: it is
one script beside the Rust grep and needs no type information.

No public API or wire change.
