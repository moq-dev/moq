# [L] A hostile peer's session cannot panic, leak, or disturb another broadcast

## Goal

A fuzz target per protocol drives a whole moq-net session, as a relay runs
it, against a peer that does anything: any message in any order, on any
stream, with resets, stalls, and closes. It asserts that the session never
panics, that what it holds returns to its baseline once the peer is gone, and
that a bystander's broadcast on another session of the same origin delivers
every group unchanged. Nightly runs both targets, and `just check` replays
their regressions.

## Plan

Decided 2026-11-10 (maintainer, /quest-plan on what could be proved about
moq-net). Today's fuzz targets stop at the codecs and stream framing; nothing
tests the rule in `AGENTS.md` that a misbehaving peer must never panic, cost
unbounded work, or affect other broadcasts.

- ✅ moq-lite and moq-transport (rejected: moq-lite only). A relay accepts
  both from untrusted peers, and a hostile peer picks the weaker one. The
  input picks the version from those a server accepts.
- ✅ m1 (rejected: m4), since what it finds is in code that ships with the
  cut.

Shape:

- The input decodes to a script of peer actions: open a stream of a type,
  write a well-formed message with fuzzed fields or raw bytes, FIN, reset or
  stop with a code, send a datagram, advance the clock, close. Run it over
  the in-memory transport doubles on the mocked clock, single-threaded, so a
  failing input replays exactly.
- The victim is one origin serving two sessions: the fuzzed peer, and a
  bystander publishing and subscribing to its own broadcast.
- Oracles: no panic; the bystander's subscriber reads every group, in order,
  byte for byte; after the peer closes and the clock runs past every timeout,
  the origin's pool charges, live tasks, and open streams are back to what the
  bystander alone accounts for. Unbounded work shows as libFuzzer's per-input
  timeout, set low.
- Seed with transcripts recorded from the integration tests (announce,
  subscribe, a group, a fetch), so the fuzzer starts deep in the state
  machine instead of at SETUP.
- Follow `rs/moq-net/fuzz/README.md`: bodies in the `fuzz` module, a one-line
  shim each, committed regressions, a row in `nightly.yml`'s matrix. Measure
  whether five minutes reaches past setup and say what budget it needs.

Out of scope: `js/net`, where a hostile server can only hurt the tab that
dialed it.

Public API: none. Wire: none.

## Related

- [Wire fuzz coverage](/quest/m4/wire-fuzz-coverage.md) - the socket-level unsafe a peer reaches
- [Peer grant bound](/quest/m4/peer-grant-bound.md) - one known cost a peer's message can impose
