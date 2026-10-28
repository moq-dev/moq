# [S] JS IETF FETCH reports an evicted group as unknown

## Goal

When the cache evicts a group after a JS IETF FETCH or fill has listed it but
before writing it, the publisher sends End of Unknown Range (`0x10C`) for that
group on drafts 18 and 22, instead of silently skipping it, which reads as
the group not existing.

## Plan

Found while landing [moq-dev/moq#7051](https://github.com/moq-dev/moq/pull/7051),
which sends Unknown or Timed-Out markers when FILL_TIMEOUT runs out. The silent
skips are the "Gone since it was listed" path in `#answerFetch` and the
`!group` path in `#runFill` (`js/net/src/ietf/publisher.ts`).

Decided with the maintainer on 2026-10-28:

- **`0x10C` on both drafts.** Both define End of Unknown Range, and the
  FILL_TIMEOUT path already sends it on draft 18. Nothing ends the FETCH
  early, and FETCH_OK's end is unchanged.
- **In m1**, so both JS gap paths are honest at the cut.

Test on mocked time: evict a listed group between listing and writing, on
both drafts, for a FETCH and for a fill, and check the requester sees an
Unknown range rather than a skip. Check whether Rust's IETF publisher has the
same skip; if so, fix it here too or note it in the PR.

Public API: none. Wire: an evicted group is reported as unknown instead of
being left out.

## Related

- [On-demand JavaScript FETCH](/quest/m1/js-fetch-on-demand.md) - serves evicted groups when an application responder can; this covers the case where nothing can
