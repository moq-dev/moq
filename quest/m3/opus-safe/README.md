# Opus without unsafe code

## Goal

The vendored c2rust crate `rs/moq-opus` is made safe where it is: moq-audio
encodes and decodes Opus through it, and its codec logic compiles under
`#![forbid(unsafe_code)]`. Output stays correct by the bar in the Plan, and
moq-audio's public API does not change; `moq-opus`'s own API may.

Not here: new Opus features (in-band FEC, DRED, libopus 1.5+ neural PLC or
LACE), the browser side (`@moq/watch` decodes through WebCodecs), and the
platform backends (AudioToolbox, Media Foundation).

## Plan

Decided 2026-11-24 (maintainer, /quest-plan):

- ✅ Spike first, then land the winning path (rejected: committing to
  safe-ifying moq-opus up front; committing to a pure-Rust crate up front;
  dropping, since the vendored crate is contained and bit-exact with C). The
  reason: `rs/moq-opus` ([moq-dev/moq#9578](https://github.com/moq-dev/moq/pull/9578))
  is 55k lines of c2rust output from libopus 1.3.1, raw pointers throughout,
  and the decoder runs it on packets from the network.
- ✅ Four candidates: (a) make moq-opus safe module by module; (b) adopt
  or contribute to a pure-Rust crate, opus-pure first; (c) a clean port of C
  libopus to idiomatic safe Rust; (d) our own implementation written from
  RFC 6716 and RFC 8251. #9578's Decisions section surveys the existing
  crates; start from it.
- ✅ Safety bar: codec logic under `#![forbid(unsafe_code)]` (rejected:
  confined unsafe from the start; any reduction from today). SIMD, if a
  measurement demands it later, is its own small module with its own review.
- ✅ Correctness bar: the decoder passes the RFC 8251 test vectors under
  `opus_compare`, and each candidate matches, bit for bit, the C libopus
  version it ports (1.3.1 for (a), 1.6.1 for opus-pure) on moq-audio's encoder
  and loss streams (rejected: bit-exact to 1.6.1 for all, which forces (a) to
  port three upstream releases; conformance only, which would have missed the
  `silk_SAT16` bug #9578 fixed).
- ✅ The decoder is replaced first, since it faces hostile input; the
  encoder follows (rejected: both in one PR; harness only).
- ✅ The maintainer picks the winning path from the spike's numbers (rejected:
  the implementer picks), since it likely commits moq to owning an Opus
  codec for years.

Decided 2026-11-25 (maintainer, /quest-run), replacing the 2026-11-24 entry
that reserved `rs/moq-opus` for a separate safe crate:

- ✅ The safe implementation is `rs/moq-opus` itself, the c2rust crate #9578
  vendored, made safe in place (rejected: renaming #9578's crate to
  `moq-libopus` and building a separate safe `moq-opus` beside it, then
  deleting `moq-libopus`). Under path (a), its modules are converted where
  they are; under (b) to (d), the picked code replaces its c2rust internals
  behind the same crate. No second Opus crate is added or deleted.

Milestone m3: the maintainer asked for this to follow #9578, and it keeps
moq-audio's public API unchanged.

Posting to a repository outside `kixelated`, `kixel-ai`, and `moq-dev`
(such as opus-pure's) needs the maintainer's OK first.
