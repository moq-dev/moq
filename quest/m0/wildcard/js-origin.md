# [S] JS origin granularity

## Goal

`@moq/net` tracks the origin an upstream reply names at the same granularity
as Rust, and handles a second, different origin the way Rust does, so a JS
node that republishes never labels one origin's content as another's.

## Plan

[#4279](https://github.com/moq-dev/moq/pull/4279) had JS record the origin
an upstream SUBSCRIBE_OK or FETCH_OK names on the consumed broadcast's shared
state (`js/net/src/broadcast.ts`), and the latest reply wins. Rust records it
per copy of a track (`track::Provenance`) and only admits a replacement
naming the same origin. Codex asked for a conflicting name to be refused
([r4112837960](https://github.com/moq-dev/moq/pull/4279#discussion_r4112837960)).
The agent declined, since a later request for the same broadcast can
legitimately land on a front serving another origin, and left the
granularity question to the maintainer
([r4113954489](https://github.com/moq-dev/moq/pull/4279#discussion_r4113954489)).
The maintainer decided in the 09-28 merged-PR audit that JS must match Rust.

- Move the origin to the level Rust keeps it at, so two tracks of one
  broadcast served by different origins stay distinct.
- A reply naming a different origin for content already labeled follows
  Rust's rule rather than overwriting silently.
- A republished broadcast advertises, per track, the origin that actually
  served it, or a random one when nothing upstream named one.

Test the mid-life origin change JS previously absorbed, and pin that JS and
Rust agree on it in the interop suite if the scenario is reachable there.

## Related

- [Wildcard](/quest/m0/wildcard/README.md) - the line this blocks
