# [M] Reconnect settings mean the same in every layer, and GOAWAY has one name

## Goal

A reconnect or GOAWAY duration reads the same in moq-net, moq-tokio's flags
and config, `@moq/net`, moq-ffi, and moq-c: an absent value turns the bound
off, and zero means zero (give up at the first failure, retry at once, close
now). `Restart::Wait` is gone. The message a peer sends to drain a session
has one name in Rust and JS, and a session sends it with one call.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit): ✅ one
quest before the cut that deletes `Restart::Wait`, makes an absent value mean
"off" and zero mean zero in every layer, and gives GOAWAY one name and one
send call (rejected: only the variant and the zero rule; leaving it to
[the connect policy's state](/quest/m4/connect-policy-state.md) in m4, where
it would be a break after the cut). The same field name means opposite
things in a browser and in Swift today, and the cut would publish both.

Facts found by the audit, checked at `80ca9787e3`:

- `Restart::Wait` (`rs/moq-net/src/reconnect.rs:156`) is set only by tests
  (`rs/moq-relay/tests/integration/handoff.rs:115`,
  `rs/moq-tokio/src/connection.rs:2861`, `rs/moq-net/src/reconnect/tests.rs`).
  It was the cluster's policy until
  [moq-dev/moq#8596](https://github.com/moq-dev/moq/pull/8596) moved the
  relay to `Handoff` (`rs/moq-relay/src/cluster.rs:2291`), which covers the
  same address at the cost of one session.
- `moq_net::reconnect::Backoff::timeout`: "Zero gives up at the first
  failure", `None` retries forever (`rs/moq-net/src/reconnect.rs:89`), and
  JS agrees (`js/net/src/connection/reload.ts`: `null` unlimited, zero gives
  up). moq-tokio reads zero as off: "A configured zero turns the bound off"
  (`rs/moq-tokio/src/connection.rs:191`), and so does moq-ffi: "0 retries
  forever", "0 leaves it uncapped" (`rs/moq-ffi/src/session.rs:211-224`).
  [moq-dev/moq#8265](https://github.com/moq-dev/moq/pull/8265) moved the core
  to `Option` and left these two layers behind.
- `Goaway::with_timeout(Duration::ZERO)` means no deadline
  (`rs/moq-net/src/goaway.rs:79`), and its one production caller
  special-cases it: "the opposite of what was asked for"
  (`rs/moq-relay/src/shutdown.rs:391`). `Server::with_timeout` in the same
  crate takes `Option<Duration>`.
- Rust has three public types named `Goaway`: the message
  (`rs/moq-net/src/goaway.rs:34`), the reaction policy
  (`rs/moq-net/src/reconnect.rs:116`), and moq-tokio's flags
  (`rs/moq-tokio/src/connection.rs:290`). JS calls the message `Drain` and
  the policy `GoawayProps` (`js/net/src/connection/goaway.ts:14,36`).
- `Session::drain()` returns a `goaway::Producer` whose only method is
  `send`, and it hands out another each call (`rs/moq-net/src/session.rs:422`).

For the PR:

- Delete `Restart::Wait`, its branch (`reconnect.rs:603`), and its tests; the
  relay handoff test that wants the old session to stay ignores the GOAWAY
  instead. Run `/verify-reconnect`, since the model covers the restart rule.
- Each layer needs three states for `max` and `timeout`: unset (the
  default), off, and a value. JS already has them (`undefined`, `null`, a
  duration). Recommended: moq-tokio's flags and config keep "unset" as the
  default and take a word (`none`) for off, refusing nothing that parses
  today except that `0` now means zero; moq-ffi's `MoqBackoff` and moq-c's
  `has_*` flags spell off as the absent value and document the default.
  Whatever the spelling, write it once in `doc/` and mirror it.
- `connect.backoff.{max,timeout} = 0` changes meaning in existing config
  files. The migration note names it; a warning at load when either is zero
  is worth one release.
- `Goaway::with_timeout(Option<Duration>)`, with zero closing at once.
- One name for the received message in Rust and JS, and one send call.
  Recommended: `Goaway` stays the message in Rust and JS renames `Drain` to
  it; the policy type becomes `reconnect::Handover` (its fields are
  `redirect` and `handover`); `session.goaway(Goaway)` replaces
  `drain().send(..)`, and `draining()` stays. The names are the PR's to
  settle, mirrored in moq-ffi and moq-c.
- Leave what [a first session refused at SETUP](/quest/m1/first-dial-refusal.md)
  owns ([moq-dev/moq#8817](https://github.com/moq-dev/moq/pull/8817)).

Tests: one table of (unset, off, zero, value) per field run against
moq-net, moq-tokio's config parse, JS, and moq-ffi, asserting the same
behavior from each.

Public API: breaking in Rust (`Restart`, `Goaway::with_timeout`, the
`Goaway` names, `Session::drain`), JS (`Drain`), moq-ffi, and moq-c.
Config and CLI: `connect.backoff.max` and `connect.backoff.timeout` of zero
change meaning. Wire: none.

## Related

- [The connect policy's state lives in one place, in both languages](/quest/m4/connect-policy-state.md) - reworks the same policy after the cut
- [A first session refused at SETUP fails fast](/quest/m1/first-dial-refusal.md) - changes the first dial's rule in the same files
