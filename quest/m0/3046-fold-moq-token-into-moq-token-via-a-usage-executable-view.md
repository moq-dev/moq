# [M] Fold moq-token into moq token via a Usage executable view

## Goal

Implement and verify the behavior tracked in [#3046](https://github.com/moq-dev/moq/issues/3046)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Summary

Fold the standalone `moq-token` binary into `moq token`, so there is one binary and one command surface instead of two build artifacts sharing a library.

#### Where we are

`moq-token-cli` is a lib+bin. The command surface lives in `moq_token_cli::Args`, which the standalone `moq-token` binary wraps in its own `Root` spec root and `moq-cli` nests under `moq token`. The implementation is genuinely shared, so this is not about duplicated logic. What is duplicated is the *surface*: two spec roots, two help renderings, two completion trees, two release artifacts, and two places for a flag to drift.

#### Why now

If #3030 lands, Usage has the mechanism for exactly this: an **executable view**. A view is argv0 dispatch against a single binary's own spec, so one binary can present a different root depending on the name it was invoked under. `moq-token generate ...` and `moq token generate ...` become one spec with two surfaces, and help renders the right prefix for each (`page_view` / `render_failure_view` already exist for this).

The blocker today is not the CLI plumbing, it is that they are separate build artifacts. Views cannot span two binaries.

#### Sketch

- `moq-cli` declares `#[usage(view("moq-token", bin = "moq-token", root = "token"))]`.
- Ship `moq-token` as a symlink, hardlink, or a thin renamed copy of `moq`, rather than as its own compiled binary.
- Drop `moq-token-cli`'s `Root` struct; the crate keeps exporting `Args` for `moq-cli` to nest.
- `moq-token` (the library) is unaffected -- it stays free of CLI concerns either way.

#### Things to decide

- **Packaging.** `moq-token` is currently released as its own artifact with its own version. A symlink changes what the release workflow produces and what a package manager installs. This is the bulk of the work.
- **Binary size.** `moq-token` today is a small binary; making it an alias of `moq` means anyone who wants only token tooling pulls the full media router.
- **Whether the standalone name survives at all**, or whether `moq token` simply becomes the only spelling after a deprecation period. That is the simpler end state if nobody depends on the separate binary.

The second point may be the one that kills it. Worth measuring `moq` vs `moq-token` stripped sizes before committing.

#### Depends on

\#3030 (Usage migration). Without it there is no view mechanism, and the shared-`Args` arrangement we have is about as good as clap allows.

## Closes

- [#3046](https://github.com/moq-dev/moq/issues/3046) - close this issue when the quest finishes
