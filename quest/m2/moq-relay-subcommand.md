# [S] Relay verb

## Goal

`moq relay` runs the relay with exactly the flags, environment, and TOML
`moq-relay` accepts, and `moq-relay` stays published as its own minimal
binary: someone who only wants a relay does not download the capture, encode,
decode, and render stack.

## Plan

The same shape `moq token` already uses: `moq-token-cli` is lib+bin, its
`Args` nested by moq-cli (`rs/moq-cli/src/main.rs`), its binary untouched.
`moq-relay` is lib+bin as well (`rs/moq-relay/Cargo.toml`), with a 15-line
`main.rs` that installs the aws-lc-rs provider, optionally jemalloc, and calls
`Relay::load(Config::load()?).await?.run()`.

- moq-cli gains a default-on `relay` feature depending on the moq-relay
  library and a `Command::Relay(moq_relay::Config)` verb, answered before any
  transport binds like `token` and `completion`, with `MoqSide::reject`
  covering it. The relay's own features (`iroh`, `cluster-lan`, backends,
  `websocket`) forward from the CLI's features of the same name.
- `Config::load` parses argv, reads the TOML `file` names, and merges the
  layers in one step (`rs/moq-relay/src/config.rs`, `parse_and_merge`), so a
  nested `Config` would carry only the flags and ignore its TOML. Split the
  file read and precedence merge into a step that takes an already-parsed
  `Config`, used by both `Config::load` and the verb, and test the same TOML
  invocation through both binaries.
- Move the provider install out of both mains into `Relay::load`, so an
  embedder and the two binaries share one startup. jemalloc stays a
  binary-level global allocator.
- `moq relay --help` renders the relay's whole tree under the verb; check the
  spec dumps both binaries produce agree.
- Docs: `doc/bin/relay/` names both spellings once, `doc/bin/cli.md` gains the
  verb, and the demo recipes keep using `moq-relay`.
- Test: `rs/moq-relay/tests/released_cli.rs` drives the binary; add a case
  that runs the same smoke through `moq relay`.

Starts on `main` after the merge: the CLI's verbs live on `usage`, which only
`dev` has.

## Required

- [Merge dev](/quest/m1/merge-dev.md) - builds on dev-only code that reaches `main` with the merge

## Related

- [One LAN mesh](/quest/m1/lan-mesh.md) - the CLI already hosts the relay library for its cluster
- [`moq` serves like a relay](/quest/m2/cli-serve.md) - the end state where the relay is `moq` with listening on by default
