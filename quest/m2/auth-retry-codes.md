# [S] AUTH_ERROR says which refusals a presenter retries

## Goal

Whether a presenter presents a refused token again follows a rule the lite-07
draft writes down, driven by the AUTH_ERROR code it received, and Rust and JS
apply it from the decoded code rather than by classifying a local error.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md).
Rust's `transient` and `verdict` (`rs/moq-net/src/auth.rs`, and
`lite/session.rs`'s `protocol_error`) and JS's `js/net/src/auth_session.ts`
retry a refused token only when the refusal maps to `Internal`,
`TooManyRequests`, or `Timeout`. The draft's AUTH_ERROR section says nothing
about retrying, so a third-party acceptor, or an embedder calling
`Request::reject`, whose auth backend is down and which answers UNAUTHORIZED
stops the client for good, while INTERNAL would have retried; only moq-relay
knows to answer `Internal`. lite-07 freezes this behavior at the cut.
[moq-dev/moq#8618](https://github.com/moq-dev/moq/pull/8618) removed the same
pattern from the reconnect loop for AGENTS.md's "never match on errors".

It changes no behavior and makes the contract visible to
other implementations.

Decided 2026-11-14 (maintainer, pick on [moq-dev/moq#8764](https://github.com/moq-dev/moq/pull/8764)): ✅ (a) write today's three codes (the session error codes Rust's `Internal`, `TooManyRequests`, and `Timeout` encode) into the draft as "not decided, present again after backoff", and have Rust and JS pass the decoded code to the retry decision (rejected: (b) every refusal final; (c) retry every refusal).

Public API: none. Wire: lite-07-wip's AUTH_ERROR gains a retry rule.

