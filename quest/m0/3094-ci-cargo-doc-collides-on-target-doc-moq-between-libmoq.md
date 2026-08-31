# [M] CI: cargo doc collides on target/doc/moq between libmoq and moq-cli

## Goal

Implement and verify the behavior tracked in [#3094](https://github.com/moq-dev/moq/issues/3094)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

The `Check` job fails intermittently in `cargo doc` with an error that names an unrelated crate, most recently on moq-dev/moq#3091 and moq-dev/moq#3066:

```
warning: output filename collision at /home/runner/work/moq/moq/target/doc/moq/index.html
error: failed to remove directory `/home/runner/work/moq/moq/target/doc/moq`
    No such file or directory (os error 2)
```

#### Cause

Two crates declare a doc target named `moq`:

- `rs/libmoq`  -  package `libmoq`, `[lib] name = "moq"`
- `rs/moq-cli`  -  package `moq-cli`, `[[bin]] name = "moq"`

Both render to `target/doc/moq/`, so `just check`'s `cargo doc --locked --no-deps` writes them to the same path. Reproduces on demand:

```
cargo doc --no-deps -p libmoq -p moq-cli
warning: output filename collision at .../target/doc/moq/index.html
```

The collision is deterministic; whether it escalates from a warning to the hard error depends on which target cleans the directory first, which is why it only sometimes fails. `RUSTDOCFLAGS="-D warnings"` does not catch it, since cargo emits the collision, not rustdoc.

It surfaces on any PR whose diff selects both crates. `just check` is diff-aware, so a change to `moq-net` pulls in both as dependents, which is why this shows up on unrelated networking PRs and points at `moq-cli` internals (`moq/args/enum.ExportSink.html`) that the PR never touched. A re-run usually passes, which makes it easy to write off as a flake.

#### Options

- Give one of them a distinct doc target: `moq-cli`'s binary is the user-facing `moq` command, so renaming `libmoq`'s `[lib] name` is the less disruptive side, though it is a published C staticlib and the name is load-bearing for consumers.
- Or document them in separate `cargo doc` invocations in `just check`, which keeps both names and costs one more pass.

Filing rather than fixing: which name gives way is a call about the published surface, not a mechanical fix.

## Closes

- [#3094](https://github.com/moq-dev/moq/issues/3094) - close this issue when the quest finishes
