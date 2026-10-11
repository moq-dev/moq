# [L] Retired handles never request a replacement's content

## Goal

A Rust or JS broadcast or track handle keeps its resolved instance for
TRACK, SUBSCRIBE, and FETCH. Once that instance retires, a new wire request
cannot resolve the replacement by path. Existing reads and cached content
from the old instance remain usable within their normal lifetime and budgets.

## Plan

Reproduced in the 2026-10-10 resume audit: resolve catalog and media from A,
publish B at the same path, wait for the replacement announcement, then
FETCH a missing group through A's held media track. Rust returns B's bytes
on lite-05/06, epochless lite-07, and IETF drafts 14-22. Explicit-epoch
lite-07 refuses the stale request. JS also accepts a new TRACK_INFO through
a held epochless broadcast after End+Start on lite-06/07.

Decided by the maintainer in that audit:

- This correctness fix gates m0. Supporting epochless peers on updated
  relays remains required; neither new paths nor the IETF epoch extension
  is a prerequisite.
- Preserve instance validity through every handle and deferred operation,
  including a track handed out before retirement, coalesced requests, and
  a request whose setup races withdrawal. A fresh request may proceed only
  when it still names the held instance. An explicit wire epoch can prove
  that; an epochless path after retirement cannot.
- Keep already-open reads sticky and allow old cached bytes. Do not close
  every old subscription to prevent new requests. A rejected operation must
  settle its waiter and release its demand, without migrating it to B.
- Same explicit epoch permits route replacement and recovery. Absent
  epochs never prove continuity. Before an epochless peer observes a
  replacement, its wire cannot identify the old instance; do not claim
  this fixes arbitrary legacy caches or that unavoidable ambiguity.

Fix validity at the request/source boundary shared by these operations,
rather than patching individual exporters. `Source` handle pinning and the
JS consume cache fix select the right initial handle but do not prevent a
surviving handle from opening new path-based wire work. Coordinate with
those active quests without duplicating their cache-selection changes.
IETF standalone FETCH resolves a path, whereas joining FETCH reads the
saved subscription cache; preserve that distinction and check both.

Add mocked-time regressions with A still alive after B is announced: held
catalog plus held media FETCH, late track lookup, re-subscription, pending
TRACK/FETCH setup, concurrent old/new consumers, exact and covering routes,
and source withdrawal without replacement. Test matching, different, and
absent epochs. Keep the explicit lite-07 epoch control and the failing
lite-05/06, epochless lite-07, and IETF14-22 matrix in CI; cover supported
legacy request forms and refusals too. Run `just check` and
`just test interop --all`. Benchmark any routing or invalidation fanout over
both routes and held consumers.

Public API: corrected request lifetime and refusal behavior; preserve
readable old subscriptions. Wire: none; published framing is unchanged.
Update existing identity and request-lifetime documentation inline.

## Related

- [Source pin](/quest/m0/broadcast-epoch/source-pin.md) - the exporter holds the exact catalog broadcast
- [JS consume identity](https://github.com/moq-dev/moq/pull/5254) - fresh consumers select the serving announcement's identity
- [JS restart keeps the request](/quest/m0/broadcast-epoch/js-restart-keeps-request.md) - an existing resolved request stays sticky
- [IETF epochs](/quest/m1/ietf-epochs.md) - explicit wire identity removes the epochless ambiguity
- [Pipelined first FETCH](/quest/m1/pipeline-requests/fetch.md) - staged requests must retain the same instance validity
