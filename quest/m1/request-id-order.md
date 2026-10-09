# [S] A reused moq-transport Request ID is refused

## Goal

On drafts 14 to 16, `@moq/net` and `moq-net` refuse an incoming request whose
Request ID is not above every ID the peer already used, instead of accepting
any ID below the window's bound. A peer can no longer reuse an ID that named
an earlier request.

## Plan

Found while landing [#5011](https://github.com/moq-dev/moq/pull/5011); it
predates it and covers every request type. `js/net/src/ietf/adapter.ts`
deliberately tolerates reuse today ("A peer reusing IDs below the bound still
cannot hold more than the window open"), bounding only how many are open.

- First confirm what drafts 14 to 16 require of an out-of-order or repeated
  Request ID and which session error they name; follow that.
- Keep a high-water mark per peer and refuse anything at or below it, in both
  languages, beside the existing window checks.
- Tests: a repeated ID and a lower ID are each refused with the drafts' error;
  in-order IDs and the MAX_REQUEST_ID grant are unchanged.

Public API: none. Wire: none (a stricter receiver).
