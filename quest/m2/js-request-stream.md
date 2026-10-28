# [S] JS IETF requests own their stream

## Goal

js/net's IETF request cleanup lives in one place: `RequestStream` owns the
request's deadline and its cancellation, and a malformed TRACK stream ends
the session as it does in Rust.

## Plan

- **One owner.** `Deadline` and SUBSCRIBE's `#cancelSubscribe` each clean up
  part of a request.
  [moq-dev/moq#6133](https://github.com/moq-dev/moq/pull/6133) kept them
  apart because routing only SUBSCRIBE's timeout through `Deadline` would
  send UNSUBSCRIBE twice. Fold both into `RequestStream`, so each request
  sends its withdrawal once on timeout, abandonment, or error.
- **TRACK trailing bytes.** JS resets a TRACK (0x6) stream that carries
  bytes after its request, while Rust closes the session
  ([moq-dev/moq#6158](https://github.com/moq-dev/moq/pull/6158)). Match Rust.

Public API: none (`Deadline` and `RequestStream` are internal). Wire: none.
