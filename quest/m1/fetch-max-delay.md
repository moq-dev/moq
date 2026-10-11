# [L] FETCH enforces publisher and reader age limits

## Goal

A historical FETCH obeys publisher `max_age` and an optional reader
`max_delay` in Rust and JS, including while a returned group is being read.
Omitting max-delay uses publisher max-age alone. An explicit delay can
only tighten the publisher's limit. Range subscriptions that replace FETCH
preserve these semantics.

## Plan

Decided in the 2026-10-10 resume audit: FETCH is not exempt from content
age limits. The maintainer explicitly requested max-delay on FETCH. The
public Rust `group::Fetch` and JS `FetchGroupOptions` have no such option
today, and FETCH readers bypass the live subscription's expiry guard.

- Add optional `max_delay` / `maxDelay` to the request surfaces and carry
  it through local, relayed, and resumed fetches. Omission means no extra
  reader cap, not SUBSCRIBE's zero-delay default. An explicit zero remains
  a zero freshness budget. Neither omission nor a large reader budget
  overrides publisher retention.
- Use the settled successor-based staleness rule from One max_age meaning:
  either wall age or media drift can expire a superseded group; the newest
  group has no successor and retains that exception. Apply the budget to
  cache hits, misses, in-flight recovery, and handed-out readers. A pending
  upstream fetch cannot suppress a deadline wake or resurrect expired data.
- Shared FETCH work must not let a tolerant caller relax another caller's
  budget, nor let one expired/cancelled caller kill data another still needs.
  Reuse the model's existing per-reader budget and aggregate-demand rules.
- Preserve published lite and IETF framing. Enforce the budget locally on
  every supported wire. Add the field to unpublished lite-07 FETCH if it
  still exists when this lands; if ranges already replaced it, carry it
  through their existing budget instead. Do not reintroduce removed FETCH.
- IETF upstream waiting uses native FILL_TIMEOUT where supported, through
  the separate quest below. It is an approximation for waiting, not proof
  of freshness. Exact content-age enforcement stays local. No custom
  IETF max-delay extension is planned.
- Check the IETF adapter's [MAX_CACHE_DURATION](https://www.ietf.org/archive/id/draft-ietf-moq-transport-22.html#section-10.3) interpretation: that native
  property bounds caching from object receipt, not from a group's successor.
  Honor the native publisher bound as well; do not claim numeric conversion
  into `max_age` makes the two clocks identical or permits retaining an IETF
  object past its native bound.

Update all fetch callers and exposed bindings under Cross-Package Sync,
including CLI, relay, archive and media readers. Keep the API cohesive with
range subscriptions rather than adding a parallel historical-only budget.
Archive replay still works for content its replay publisher makes available
within its declared retention; stale live-cache data is not an exemption.

With mocked time, test omitted, zero, tighter and looser budgets; cache hit
and cold fetch; timed/untimed tracks; no writes after a successor; reordered
or aborted successors; reading past expiry; route replacement; and coalesced
callers with different budgets and cancellation. Test publisher max-age
clamping at each supported wire boundary and IETF native retention clocks.
Run existing Rust/JS and `just test interop --all` lanes, `just check`, and
`just drafts check` for lite changes. Benchmark budgeted fanout if the
implementation changes shared expiry or request bookkeeping.

Public API: new optional FETCH budget across Rust, JS and exposed wrappers;
historical reads can now expire. Wire: only unpublished lite-07 gains a
field if needed; published formats are unchanged. Update API, concept,
CLI and matching draft documentation inline, including the changed FETCH
exemption and the distinction between age and upstream wait budgets.

## Required

- [One max_age meaning](/quest/m1/cache-max-age.md) - one expiry rule and deadline wake mechanism

## Related

- [IETF fill timeout](/quest/m1/ietf-fill-timeout.md) - upstream waiting bounded through the existing FETCH parameter
- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - historical ranges retain this optional budget and publisher clamp
- [Retired requests](/quest/m0/broadcast-epoch/retired-requests.md) - identity stays pinned independently of age
