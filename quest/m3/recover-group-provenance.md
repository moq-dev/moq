# [M] A recovered group's too-old verdict is judged by the request that delivered it

## Goal

When a group read switches routes partway, `Recover` (`rs/moq-net/src/model/resume.rs`, `js/net/src/resume.ts`) decides whether that group's later OLD stands from the request that actually delivered it (the copy's live subscription or a fill/fetch), not from how recovery found it. Two cases left open by [moq-dev/moq#9768](https://github.com/moq-dev/moq/pull/9768) then behave, both needing a mid-group switch between copies whose upstreams asked a narrower budget than the reader:

- A recovery started from a non-OLD failure (such as CANCEL) that adopts its own fetch's answer through the copy's cache treats that group as fetched, so a later OLD on it stands instead of giving the route one more request.
- A group adopted from the second route's live subscription, after looking past the first route's OLD, is not treated as judged: if that live subscription later resets it OLD under a budget narrower than the reader's, the reader fetches from that route instead of ending the group OLD.

Rust and JS match. Public API: none. Wire: none.

## Plan

Accepted on [moq-dev/moq#9768](https://github.com/moq-dev/moq/pull/9768) by the maintainer (2026-11-27) to land the single-route fix first and carry these here, since the fix belongs in the relay's copy in `rs/moq-net/src/model/track.rs`, which [moq-dev/moq#9719](https://github.com/moq-dev/moq/pull/9719) rewrote.

- Record on the copy, per group, which request delivered it (live subscription or fill), and have `Recover` set `judged` from that record when it adopts a replacement, replacing today's `replacement.fetched || excusing` (Rust `resume.rs`, JS `#judged`). Keep the one-more-request-per-group bound the PR's `a_fetched_groups_stale_reset_stands` and `a_group_fetched_from_another_route_stands_on_its_stale_reset` check.
- Bound the record by what the copy already holds per group: no state outlives the group in the copy's cache.
- Judge an OLD drop by the `Max Age` it carries rather than the copy's narrowest `judging`; resets still need `judging`.
- Extend `rs/moq-net/verify/recover/StaleRecover.tla` with a second route and the delivering request, so the two cases above fail `AnswersAsUpstream` or `AsksOnlyWhenWider` without provenance and pass with it.
- Add a failing-first Rust and JS test for each case, with mocked time.

## Related

- [A widened resumed ordered reader asks its copies' stale give-ups again](/quest/m3/widened-front-reader-asks-stale-again.md) - also re-judges a copy's stale verdicts across route switches
