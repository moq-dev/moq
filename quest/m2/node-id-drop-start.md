# [M] NodeId drops Start, and a Relay ID names one run

## Goal

lite-07's NodeId is a Relay ID and the marked bit, on the wire and in Rust
and JS, with no Start. A Relay ID is random per process, and no API pins
one. Floors key by relay alone, `Error::Regressed` and its wall-clock start
go away, and the draft says a Relay ID names one run of a relay.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the main → release routing
audit): ✅ drop Start, with a random Relay ID per run and no pinning
(rejected: keeping Start and pinning, with Start from a counter the embedder
persists; leaving both and fixing only the draft's wording). It is breaking
on the wire and the API, free before lite-07 freezes, and the cut waits on
it.

Why:

- Every NodeId on the wire carries (relay, start, marked), so Start costs an
  8-byte varint per hop.
- moq-relay mints a random Relay ID per process (`doc/bin/relay/cluster.md`,
  about line 303, says nothing pins it), and handoff relies on the old and
  new processes having distinct IDs
  ([relay-handoff-routes](/quest/m4/relay-handoff-routes.md)). moq-ffi uses
  the default, and JS does not route, so Start never decides anything in a
  real deployment.
- Only a Rust embedder can pin an ID today, through `origin::Config::node`
  (`rs/moq-net/src/model/origin.rs:100-107`, `:201`). For one that does,
  Start comes from the wall clock (`route/node.rs:350-357`). After an NTP
  step back, a peer refuses the relay with `Error::Regressed`
  (`route/relay.rs:1014-1018`) until that peer restarts. A relay reached
  through a neighbour gets no check at all, so its routes are refused
  silently.
- The draft says a Relay is "the same across its restarts" (about line
  1095), which moq-relay contradicts on purpose.

Notes for the implementer:

- `random_id` keeps generated IDs under 2^53 for older `@moq/lite` clients
  that decode hop IDs as JavaScript numbers (origin.rs:228-234). Without
  Start, uniqueness rests on that randomness alone. Say in the PR whether
  53 bits are enough, given that a collision merges two relays' routes.
- `Config::node` also names the origin in lite-06 hop chains. Decide whether
  it stays as a test-only constructor or goes; nothing outside tests sets it.
- Update the route-algebra proof's vectors, the TLA+ models that name Start,
  and JS parity. A wire change runs `just test interop --all`.

Public API: `origin::Config::node` and the pinning it allows go; `NodeId`
loses `start`. Wire: NodeId loses Start (lite-07-wip). Migration note: a
Relay ID can no longer be pinned.
