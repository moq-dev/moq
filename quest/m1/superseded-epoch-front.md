# [M] A superseded epoch never answers a fresh request

## Goal

Once a higher Epoch of an Instance wins a path, a request made after that
resolves only through that Epoch, in Rust and JS: a front still held through
the older Epoch keeps serving the readers it already serves (claims of one
Instance serve the same Broadcast, so they move onto the new Epoch unseen),
but never admits a fresh request, and an answer the older Epoch gives to a
request still in flight never resolves it. A request selecting a newer Epoch
that is unavailable fails with UNROUTABLE, as the moq-lite-07 draft's
Resolution says ("an older Epoch never stands in for an unavailable newer
one"), instead of resolving on the older Epoch and ending once the newer one
refuses.

## Plan

Found by CodeRabbit on
[moq-dev/moq#9371](https://github.com/moq-dev/moq/pull/9371) and read from
the source, not yet run. Both languages key a front's continuity on the
Instance alone, which predates that PR within one filing; that PR's
cross-filing ranking only adds the case of the two Epochs filed at different
depths.

- JS (`js/net/src/origin.ts`, `OriginState.route`): with a front held
  through Epoch 1 and Epoch 2 of the same Instance selected but not yet
  answering, `kept` hands the Epoch-1 front to the path's slot, and a fresh
  `request()` joining that slot resolves on it.
- Rust (`rs/moq-net/src/model/origin.rs`, `FrontServe::stale`): with a request to
  Epoch 1 in flight when Epoch 2 arrives, `stale` compares only the Instance,
  so the Epoch-1 answer resolves the front, and the next selection moves it to
  Epoch 2.

Reproduce each first as a failing test: claim `src` filed at `src/a`
(Above 1) at Epoch 1, a request for `src/a/b` held through it, then Epoch 2
of the same Instance announced at `src` and marked unavailable before it
answers; a fresh request must fail UNROUTABLE, and the held reader keep
reading. Recommended: record the Epoch a front resolved through, and admit a
fresh request (JS) or accept an answer (Rust) only from the selected Epoch,
keeping same-Epoch route changes as make-before-break.

Public API and wire: none. Behavior: a fresh request no longer resolves
through a superseded Epoch.
