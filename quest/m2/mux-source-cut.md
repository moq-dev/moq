# [S] A library recorder gets the takeover cut

## Goal

A recorder built directly with `moq_hls::export::Broadcaster::new` ends a
replaced instance's recording at what it listed when another publisher takes
the broadcast over, exactly as one built through `Server::broadcaster` does.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7399](https://github.com/moq-dev/moq/pull/7399): a separate S
quest in m2 for the `moq_mux::Source` API, alongside reporting stalled
segments.

[moq-dev/moq#7399](https://github.com/moq-dev/moq/pull/7399) (open) detects takeover in the server's follower (`Followed::start`
in `rs/moq-hls/src/server/mod.rs`) and cuts through the crate-private
`Broadcaster::replaced`. `Broadcaster::new(source, config)`
(`rs/moq-hls/src/export/mod.rs:113` on origin/main) pins one instance and
never sees the takeover, because `moq_mux::Source` exposes no way to follow
its own path: `Source::follow(None, ..)` returns
`follow::Instances::fixed` for the source's own path
(`rs/moq-mux/src/source.rs:379`).

- Give `Source` a public way to follow its own path's announcements (a
  `follow_self`, or `follow` returning live instances for the own path when
  the source was built from an origin), reusing `follow::Instances`; do
  not add a second follower.
- Have `Broadcaster` watch it and call its existing cut on the first
  replacement, so `Server` and library callers share one path; drop the
  server-side special case if it becomes redundant.

Test: a `Broadcaster::new` recording replaced by a new epoch drains exactly
its listed segments and returns `Ok(None)`, mirroring [moq-dev/moq#7399](https://github.com/moq-dev/moq/pull/7399)'s
`a_replaced_recording_ends_at_what_it_listed`.

Public API: `moq_mux::Source` gains a way to follow its own path. Wire:
none.

## Related

- [Finite drain for a replaced run's HLS recording](/quest/m2/hls-restart-drain-cutoff.md) - [moq-dev/moq#7399](https://github.com/moq-dev/moq/pull/7399) lands the cut this extends
- [A recording stalled on an open group reports what it waits for](/quest/m2/hls-stalled-segment.md) - the other [moq-dev/moq#7399](https://github.com/moq-dev/moq/pull/7399) follow-up
- [Hang contribution references](/quest/m2/contribution-ref.md) - also reuses `Source`'s pinning and `follow`
