# [M] A moq-lite-06 subscription survives a GOAWAY handover

## Goal

A client on moq-lite-06 (today's default) keeps a live subscription through
a GOAWAY handover: once the replacement session serves the broadcast, the
track continues from it instead of ending when the drained session closes.

## Plan

Found by [moq-dev/moq#9176](https://github.com/moq-dev/moq/pull/9176) while
fixing `moq-tokio`'s `a_fleet_drain_redials_through_a_fresh_resolve`. Over
lite-06 the wire carries no instance, so the subscriber's origin mints one
per announcement. The front serving the track is pinned to the drained
session's instance (`pick` in `rs/moq-net/src/model/origin.rs`), so it
never selects the replacement's route. When the drained session closes, the
front ends `Unroutable` and the reader sees `GoawayTimeout`, even with the
replacement up and announcing. Over lite-07, which carries the instance, the
same test's track continues on the replacement. The comments in
`rs/moq-net/src/reconnect.rs` (the GOAWAY handling in `ended`, and the
module doc on the fallback probe) say live tracks splice onto the replacement,
which holds only for lite-07.

A possible fix: the connect policy knows the replacement is a handover of the
same peer, so its announcements of a path the predecessor serves could inherit
the predecessor's instance. Check first whether a same-path announcement from a
redial of the same URL is safe to read as one instance (a fleet name can land
on a relay serving a restarted publisher).

## Waiting

- Maintainer: fix it for lite-06, or only correct the `reconnect.rs` comments
  and leave lite-06 subscribers to re-subscribe until lite-07 is the default.
  Recommended: correct the comments now and fix it, since a relay drain ends
  every live subscription of a lite-06 client today. An agent can't decide this: it
  depends on how soon lite-07 becomes the default.

## Related

- [GOAWAY migration follow-ups](/quest/m3/goaway-migration-followups.md) - closes an idle predecessor once the replacement has synced
