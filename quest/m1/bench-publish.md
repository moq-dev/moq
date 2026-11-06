# [M] Publish moq-bench to crates.io

## Goal

`cargo install moq-bench` installs a single `moq-bench` binary: plain
`moq-bench` is the load generator, and `moq-bench host` is the `/proc`
sampler that runs beside the process under test. release-plz publishes the
crate like the other crates, first as 0.1.0, so relay operators can load-test
their own deployments without cloning the repository.

Out of scope: release binaries, Homebrew, and nixpkgs packages for moq-bench,
and any CLI stability promise beyond semver.

## Plan

Decided 2026-11-05 (maintainer, /quest-plan):

- Should moq-bench's three bins be combined before publishing?
  - ✅ One bin + subcommand: `moq-bench` stays the load generator with its
    current flags and TOML config, `moq-bench-host` becomes `moq-bench host`,
    and the io_uring probe leaves the published crate
  - Keep them separate
  - Merge all three, the probe as a hidden `moq-bench uring-check`
- When should it reach crates.io?
  - Main, then backport
  - Main only, ships at the cut
  - ✅ (maintainer's words) "main, only backport if easy"
- First published version?
  - ✅ 0.1.0
  - 0.0.1

Today the crate has three `[[bin]]`s (`rs/moq-bench/Cargo.toml`):
`moq-bench` (`src/main.rs`), `moq-bench-host` (`src/host.rs`, Linux-only),
and `moq-bench-uring-check` (`src/uring.rs`, 15 lines, behind the `uring`
feature). `bench/run.sh` runs the probe, `bench/profile.test.py` stubs its
binary, and `bench/metadata.py` names the `moq-bench/uring` feature in its
runtime and profile descriptions; nothing else uses the feature, so it goes
with the probe and those three callers follow. Put the probe wherever
`bench/run.sh` can still build it with the least new surface; a
`moq-uring` example is the likely fit. On a non-Linux host,
`moq-bench host` errors the way the bin does today.

Update every caller and doc in the same PR: `bench/run.sh` (`HOST_BIN` and
the probe call), `rs/moq-bench/README.md`, and any CI or audit file naming
the old bins. Then drop `publish = false` and set the version to 0.1.0.

Backport counts as easy when the port onto `release`'s moq-bench applies
without shimming any shipped crate's API. A backport changes no shipped
crate, like CONTRIBUTING's bench-only backport. Otherwise skip it, say so in
the PR, and moq-bench publishes at the cut.

The first publish goes out by token, so add moq-bench to the
[crates.io trusted publisher](/quest/m3/crates-trusted-publisher.md)
`Waiting` list (done in this planning PR).

## Related

- [crates.io trusts the workflow](/quest/m3/crates-trusted-publisher.md) - moq-bench needs a trusted publisher after its first publish
