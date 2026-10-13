# [S] JavaScript answers TRACK_STATUS

## Goal

A `@moq/net` publisher answers IETF TRACK_STATUS on drafts 18 and later
instead of refusing every one with NOT_SUPPORTED, so a native fetch-only
request learns the track in one round trip rather than falling back to
SUBSCRIBE.

## Plan

Since #4974, a native subscriber asks TRACK_STATUS for demand without a
subscription. #5409 found that the relay's IETF publisher can make a reader's
first request look fetch-only, and that against a JS publisher the refusal
failed the track; the Rust subscriber now falls back to SUBSCRIBE on
NOT_SUPPORTED, at the cost of an extra round trip.

Decided 2026-10-13: mirror the Rust publisher
(`rs/moq-net/src/ietf/publisher.rs`, `run_track_status_stream`). Check the
grant before resolving, resolve the broadcast through the serving origin like
a SUBSCRIBE (dynamic handlers included), wait for the track's info, and answer
TRACK_STATUS_OK. A requester that cancels is owed nothing and releases its
demand through the path #5411 added. Errors map to the same refusal codes as
Rust.

Split out of [JavaScript FETCH](/quest/m1/js-fetch.md), which is blocked on
JS ranges; answering TRACK_STATUS does not need FETCH.

Verify with a native IETF subscriber against a browser publisher on every
supported draft: a fetch-only request gets TRACK_STATUS_OK with no SUBSCRIBE
fallback, a cancelled request releases origin demand, and a request outside
the grant is refused. Run `just test interop --all`.

Public API: none. Wire: implement the existing TRACK_STATUS and
TRACK_STATUS_OK formats in JS; update `doc/concept/standard.md` if it lists JS
as refusing them.

## Related

- [JavaScript FETCH](/quest/m1/js-fetch.md) - serves the FETCH that follows this answer
