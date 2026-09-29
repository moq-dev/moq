# [M] Group fetch fill

## Goal

A relay filling a cache miss from an IETF upstream caches exactly what the
upstream promised, or nothing. A short or malformed fetch stream fails the
group loudly instead of landing in the cache as a complete one, and the
upstream FETCH asks for the frames the downstream reader actually wants.

## Plan

[#4276](https://github.com/moq-dev/moq/pull/4276) added the fill: each cache
miss becomes a standalone FETCH of one group (`run_group_fetch` in
`rs/moq-net/src/ietf/subscriber.rs`), and `recv_group_fetch_objects` writes
the fetch stream into the accepted group. Three of Codex's review findings
merged unanswered, and the maintainer ruled that each blocks the line:

- **Truncation is cached as complete**
  ([r4113942736](https://github.com/moq-dev/moq/pull/4276#discussion_r4113942736)).
  FETCH_OK names an `end_location`, but it never reaches the decoder. If the
  peer cleanly ends the stream early, the group is finished and cached short,
  and later readers see a normal end. Check the last object received against
  what FETCH_OK promised before finishing the producer, and abort the group
  otherwise. That only works when `end_location` names a concrete last
  object: for a whole-group request our own `run_fetch_stream` answers with
  the requested boundary (`(group + 1, 0)`), so a stream holding only object
  0 looks like a valid one-object group. For that case, find the wire signal
  that marks a complete group (an End of Group status object, or a concrete
  `end_location` from the publisher, fixing ours to send one) and require it;
  if the drafts we speak offer none, ask the maintainer rather than guess.
- **A first object with no IDs is accepted**
  ([r4113942737](https://github.com/moq-dev/moq/pull/4276#discussion_r4113942737)).
  The `(false, None | Some(1))` arm treats omitted Group and Object IDs as
  "same group, next object", which only means something when a prior object
  exists. On the first object `prior_group` is `None` and `next` is 0, so an
  anonymous object is cached under the requested group. The first object must
  carry explicit IDs resolving to the requested group and start object; refuse
  it as a protocol violation otherwise.
- **The upstream FETCH ignores the requested frame offset**
  ([r4113942733](https://github.com/moq-dev/moq/pull/4276#discussion_r4113942733)).
  `group::Request::frame_start()` carries the downstream reader's start, and
  `accept` already inserts the group at that offset, but the upstream FETCH
  always asks from object 0 and the decoder numbers from 0. An upstream that
  evicted the prefix but holds the suffix refuses a request it could have
  answered. Ask from `frame_start` and number the fill from it, so the
  request, the decoder, and the producer agree on where the group starts.

Each fix gets a regression test in the existing subscriber test module that
fails without it. Keep the whole-group-only scope of the line: this is about
filling faithfully, not about new FETCH shapes.

## Related

- [Moxygen compatibility](/quest/m1/moxygen/README.md) - the line this blocks
- [Fetch without SUBSCRIBE](/quest/m1/ietf-fetch-only.md) - also changes how a relay reaches upstream for fetches
