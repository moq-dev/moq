# [S] Refuse unknown MOQ_* environment variables

## Goal

Every binary that reads `MOQ_*` variables (`moq-relay`, `moq`, `moq-bench`,
`moq-boy`) exits at startup, naming the variable, when the environment holds
a `MOQ_*` variable the binary does not read. A deployment (Docker, systemd,
NixOS) that sets a stale or misspelled name fails loudly instead of silently
running on defaults.

## Plan

Decided 2026-10-23 (maintainer interview):

- Every binary, as an error. A shell holding one tool's `MOQ_*` variables
  while running another trips it; that is accepted.
- One helper in `moq_tokio::cli` compares the environment's `MOQ_*` names
  with those the binary's usage spec declares, plus any it reads directly,
  and each binary calls it after parsing. It scans `std::env::vars_os()`,
  since `vars()` panics on any non-UTF-8 entry, even an unrelated one.
  usage-rs has no unknown-env check yet;
  [the upstream option](/quest/m2/usage-unknown-env.md) replaces this helper
  once released.
- Turns the nix module's unit-test guard from
  [moq-dev/moq#6474](https://github.com/moq-dev/moq/pull/6474) into a check
  every deployment hits. Note the new startup error in `doc/bin/relay/` and
  `doc/bin/cli.md`.

## Related

- [Upstream unknown-env option](/quest/m2/usage-unknown-env.md) - replaces the helper
