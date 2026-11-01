# [S] The Rust IETF publisher marks a lost listed group unknown

## Goal

When the Rust moq-transport publisher has listed a group in a FETCH answer or
a draft-22 fill but can no longer serve it, it sends End of Unknown Range
(`0x10C`) for that group on drafts 18 and 22, as the JS publisher does, instead
of skipping it silently.

## Plan

Inspection 2026-10-31: this plan predates the final implementation of
[moq-dev/moq#7204](https://github.com/moq-dev/moq/pull/7204). That change removed
both listed-group loops and the JS unknown-marker behavior. Rust's
`prepare_fetch` reads the sole group into an owned `FetchedGroup` before
`serve_fetch_stream` sends FETCH_OK; JS does the same with `readFetched`.
Eviction before acceptance refuses the request, while eviction after acceptance
cannot truncate the buffered answer. There is no remaining FETCH listing/write
race to reproduce on drafts 18 or 22.

Draft-22 fills still acquire the selected head after opening FETCH_HEADER, but
both Rust `run_fill` and JS `#runFill` reset their own stream when the group is
unavailable. This is explicit failure, not the silent skip described below.
The merged PR records that fill-failure policy. Replacing it with an unknown
marker needs a new scope decision, including whether a missing head may count
as a successfully completed fill.

`Unsupported` must remain a refusal: it can mean unsupported request shape or
an IETF group representation that FETCH cannot serialize. `NotFetchable` means
the sequence is held as a datagram, not that its existence is unknown. Before
FETCH_OK either error is returned as a request refusal; a fill resets its own
stream. Neither should be hidden as unknown without a separate decision.

Recommendation to the parent: retire this quest as superseded by the settled
one-group implementation. If a fill marker is still wanted, first settle that
behavior for Rust and JS together. No production change has been made.

Found 2026-10-28 by [moq-dev/moq#7150](https://github.com/moq-dev/moq/pull/7150),
which fixed the then-current JS publisher. The original Rust diagnosis was a
skip in two places: the FETCH answer loop in `serve_fetch_stream`, on
`NotFound | NotFetchable | Unsupported | Old`, and `run_fill`, when
`fetch_group` fails with the same errors.

Rust can refetch from upstream, so a skip there meant a failed refetch rather
than only an eviction. The original proposal mapped `NotFound` and `Old` to
`0x10C` and left `Unsupported` and `NotFetchable` for the PR to decide.

Decided 2026-10-28 (maintainer): it starts after the open quests that edit
the same publisher land. Those have landed: Rust FETCH parity in
[moq-dev/moq#6999](https://github.com/moq-dev/moq/pull/6999), and the
SUBSCRIBE properties opt-out in
[moq-dev/moq#7085](https://github.com/moq-dev/moq/pull/7085). A third was
IETF fill timeout, which
[moq-dev/moq#7194](https://github.com/moq-dev/moq/pull/7194) dropped for
[one group](https://github.com/moq-dev/moq/pull/7204), which edited the same
paths and has landed. A FETCH or fill now lists at most one group, so this
marks that one group.

Original test request: a listed group evicted before it is written, for FETCH on drafts 18
and 22 and for a draft-22 fill, each failing before the fix. Run `just test interop --all` if the framing order changes.

Public API: none. Wire: none; the marker exists in both drafts.
