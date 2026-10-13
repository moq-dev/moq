# [M] A duration set in code beats the parser default

## Goal

On a moq-tokio config built by the command line or merged from settings, a
duration a caller sets in code takes effect, zero included, unless its flag or
env var was actually given. #5350 fixes this for `connect.backoff.timeout`.
The rest still ignore the code: connect `race`, `resolution_delay`, and
`timeout`; `listen.timeout`; QUIC `idle_timeout` and `keep_alive`; backoff
`initial` and `max`; GOAWAY `handover`; and WebSocket `delay`.

## Plan

The cause, from #5350: usage builds `default_value_t` by formatting and
reparsing it, so `CliDuration::fallback` reaches the struct as an explicit
value and `CliDuration::resolve` prefers it over the typed field. And a parse
leaves each `#[usage(skip)]` typed field at `Duration::ZERO`, which `resolve`
reads as unset, so a zero set afterwards looks like no value. Zero is a real
setting for most of these fields: wait forever, disable keep-alive, dial every
address at once, or give WebSocket no head start.

These aren't one-line fixes. Each field's public type changes, and its callers
in moq-c, moq-ffi, moq-gst, moq-relay, and the moq-tokio tests (about 70
sites) follow.

Decided 2026-10-13: give all ten fields #5350's shape. The typed field becomes
`Option<Duration>` (`None` is unset, and `Default` holds the default), the
`*_arg` field loses its parser default, and each field resolves the flag, then
the typed field, then the default. `fallback`, `resolve`, and the `explicit`
bit then have no callers, so delete them (also decided) and leave `CliDuration` as the
humantime parser.

Rejected: fixing usage (jdx/usage, an outside party) to assign
`default_value_t` without the string round-trip. It fixes only the first
half; skipped fields still start at zero, so the zero ambiguity stays.

Start after #5350 merges, since this reuses its `serde_duration::option` and
shape. Like #5350, help no longer prints these flags' defaults, because
usage's `default` sets a parser value; each doc comment states its default.
Use a help-only default instead if usage offers one.

Extend #5350's `a_timeout_set_in_code_beats_the_parser_default` to every field:
a parsed config, a merged round-trip with zero and nonzero values, a given
flag still winning, and a config built in code. It must fail on main.

Public API: breaking, each listed `moq_tokio` field goes from `Duration` to
`Option<Duration>`. The C ABI and FFI records keep their `has_*` flags and
don't change. Wire: none.

## Related

- [#5350](https://github.com/moq-dev/moq/pull/5350) - the backoff timeout fix this generalizes
