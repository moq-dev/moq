# [S] A relay forwards an upstream refusal's retry interval

## Goal

When a relay forwards an upstream SUBSCRIBE refusal downstream, the
downstream peer receives the retry interval the upstream sent. Today
`rs/moq-net/src/ietf/subscriber/tracks.rs` keeps only `error_code` from a
received REQUEST_ERROR, and since
[moq-dev/moq#7993](https://github.com/moq-dev/moq/pull/7993) the sent
interval is derived from the decoded error. `from_code` maps codes lossily
(DOES_NOT_EXIST decodes as `NotFound`), so an upstream "retry later" becomes
0, "never retry", at the first hop.

## Plan

Decided 2026-11-06 (maintainer, /quest-complete, a follow-up of #7993):

- ✅ **Forward it verbatim.** Keep the received interval with the refusal
  and send it downstream unchanged; derive an interval only for the relay's
  own refusals. Not chosen: making `from_code` lossless and re-deriving at
  each hop; leaving the lossy hop.
- ✅ m1: additive, no semver break.

Starts once #7993 lands. Cover it with a two-hop test in Rust, and in JS if
`js/net` relays moq-transport refusals too.

Public API: none expected. Wire: the field's value only.
