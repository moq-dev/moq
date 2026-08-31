# [M] Send valid PUBLISH_DONE statuses for every supported IETF draft

## Goal

Implement and verify the behavior tracked in [#3207](https://github.com/moq-dev/moq/issues/3207)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Problem

The Rust and TypeScript IETF adapters diverge from draft-19 when terminating ordinary subscriptions:

- Rust sends HTTP-like `200` and `500` PUBLISH\_DONE status codes, which are not registered PUBLISH\_DONE codes: [publisher.rs](https://github.com/moq-dev/moq/blob/7494084aaf7e2fa6abe553ac83101ac4ef19f33a/rs/moq-net/src/ietf/publisher.rs#L603-L640)
- TypeScript sends PUBLISH\_DONE only for drafts 14 through 16, even though drafts 17 through 19 also require it: [publisher.ts](https://github.com/moq-dev/moq/blob/7494084aaf7e2fa6abe553ac83101ac4ef19f33a/js/net/src/ietf/publisher.ts#L226-L249)

PUBLISH\_DONE is not limited to publisher-initiated PUBLISH. It is the final control message for an ordinary subscriber-initiated SUBSCRIBE as well. Removing or omitting it breaks normal subscription termination semantics. See [draft-19 section 10.11 and the section 15.11.3 registry](https://www.ietf.org/archive/id/draft-ietf-moq-transport-19.txt).

Inbound PUBLISH may remain unsupported, but it should continue to be decoded and rejected with REQUEST\_ERROR NOT\_SUPPORTED rather than becoming a session-fatal unknown message.

#### Implementation plan

1. Define a typed internal PUBLISH\_DONE status mapping for every supported IETF draft.
2. Replace Rust's `200` and `500` values with the relevant registered completion or failure codes.
3. Make TypeScript emit PUBLISH\_DONE for drafts 17 through 19 as well as earlier supported drafts.
4. Keep inbound PUBLISH decoding and graceful NOT\_SUPPORTED rejection unchanged.
5. Preserve unknown received status codes internally rather than coercing them to a known value.
6. Add Rust and TypeScript interop tests for clean completion, failure completion, and unknown received codes across the supported version matrix.
7. Update the matching IETF implementation documentation or draft notes if repository behavior documentation needs correction.

#### Branch targeting

This corrects existing wire behavior without breaking a published source API, so it targets `main`.

## Closes

- [#3207](https://github.com/moq-dev/moq/issues/3207) - close this issue when the quest finishes
