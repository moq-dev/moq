# [S] Drop a moq-transport group whose head never arrives, in either order

## Goal

When a moq-transport subscription delivers a group starting partway through
and the fill or joining FETCH that should supply its head is refused or ends
empty, moq-net drops that mid-group stream whichever arrives first, the
stream or the refusal. Today one order drops it and the other keeps the tail
as if it were the whole group.

## Plan

Decided by the maintainer: a stream starting mid-group with no head is
dropped and the subscription goes on
([moq-dev/moq#6407](https://github.com/moq-dev/moq/pull/6407), 2026-10-22).
Confirmed 2026-10-25, which rejected the earlier plan to keep the tail and
forward it to moq-transport subscribers.

Where it happens: `open_group` in `rs/moq-net/src/ietf/subscriber.rs`.

- If the stream arrives while the fill is still pending, it waits in
  `claim_fill`. Once the refusal settles the fill, `claim` returns no head,
  and the stream fails with "no fill to stitch a mid-group stream onto",
  which drops the group. This is the decided behavior.
- If the refusal lands first, the fill is no longer outstanding, and the
  stream takes the unfilled path, which opens the group from its first
  object. Only the wire metadata (`group::Wire.first_id`) records where it
  starts: the model group begins at frame 0, so it looks complete, and
  `position_group` serves the tail to moq-lite subscribers as a whole group.
  `Fill::Done`'s doc already says such a stream "gets dropped"; the code
  does not.

Drop it in the second order too. The unfilled path also serves subscriptions
that never requested a head, so tell a refused or failed fill apart from no
fill at all rather than dropping every stream whose first Object ID is
nonzero.

Cloudflare's moq-test-client reproduces both orders: it writes object 0
before SUBSCRIBE_OK and refuses the joining FETCH
([cloudflare/moq-rs#281](https://github.com/cloudflare/moq-rs/issues/281)).

Tests: a subscriber unit test for each order, with the fill refused before
the mid-group stream arrives and after it. Both drop the group, and the
subscription keeps delivering whole groups after it.

Public API: none. Wire: none.
