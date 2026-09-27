# Moxygen compatibility

## Goal

More compatibility with moxygen's moq-test suite, never a full pass. A peer
that speaks one subgroup per group, FETCH of whole groups, and one datagram
per group gets those through the relay.

## Plan

`conformance_test.sh` on draft-16 scored 0/76 against moq-relay 0.15.1.
Subgroup subscribes did return objects. The relay decodes IETF into the
moq-lite track and writes a new subgroup. A green run of all 76 cases is not
the exit test.

Out of scope, so they are not reopened as bugs:

- PUBLISH. The relay refuses it. Routing stays SUBSCRIBE per namespace.
- Subgroups. A non-zero subgroup id is dropped. One stream per group.
- Per-group priority. `track::Info` has one priority. A 200/201 split by
  group parity stays a gap.
- Foreign object extensions. Dropped. A timestamp we add is ours.
- The end-of-group bit. Optional in draft-16. The group's one stream FINs
  at the end, so the bit adds nothing this line will do.
- Several datagram objects in one group.
- A peer that answers SUBSCRIBE_NAMESPACE with unimplemented. The session
  already continues.

Docs stay inline in the change that makes them stale. No new guide.

## Quests

- [Group FETCH](/quest/m1/moxygen/fetch.md) - an IETF FETCH of whole groups is served from cache or fetched upstream, one group at a time
- [Datagram groups](/quest/m1/moxygen/datagram.md) - an IETF datagram that is one object in a group arrives as a moq-lite datagram group

## Related

- [JavaScript FETCH](/quest/m1/js-fetch.md) - the browser publisher answers a FETCH this relay forwards
- [Track priority scope](/quest/m1/track-priority-scope.md) - send-order fairness, not the default value
