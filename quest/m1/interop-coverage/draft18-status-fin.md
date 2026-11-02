# [S] A draft-18 TRACK_STATUS followed at once by a FIN is answered

## Goal

On draft 18, a moq-net publisher (Rust and `@moq/net`) answers a TRACK_STATUS whose requester FINs right after sending it, with TRACK_STATUS_OK or REQUEST_ERROR, instead of resetting the stream with CANCELLED. A reset or STOP_SENDING still withdraws it. Every other draft-18 request keeps treating a requester FIN as a cancel. A unit test sends TRACK_STATUS plus an immediate FIN on drafts 18, 20 and 22 and gets an answer on each. `doc/concept/standard.md` no longer says a reader whose publisher refuses `TRACK_STATUS` still subscribes, where that reader only queries or fetches.

## Plan

Decided 2026-10-31 (maintainer, `/quest-import` of [moq-dev/moq#7438](https://github.com/moq-dev/moq/issues/7438) and [moq-dev/moq#7423](https://github.com/moq-dev/moq/issues/7423)).

moxygen FINs right after TRACK_STATUS on draft 18+, since nothing can follow one. The WG agrees this is normal (moq-wg/moq-transport#1634), and draft 19 §3.3.2 says a FIN is not a cancellation. On draft 18, `rs/moq-net/src/ietf/request_stream.rs:11` `fin_cancels` treats any requester FIN as a withdrawal. So a TRACK_STATUS forwarded by a moxygen-based relay such as moqx is reset, and a moq-lite viewer behind moq-relay never subscribes.

- Scope of the FIN rule:
  - ✅ TRACK_STATUS only: a FIN only ends a TRACK_STATUS requester's input, on every draft. Our own draft-18 requesters keep cancelling other requests by FIN.
  - FIN never cancels on draft 18 (except SUBSCRIBE_NAMESPACE/SUBSCRIBE_TRACKS, §6.1)
  - Won't fix
- The fix is in [moq-dev/moq#7439](https://github.com/moq-dev/moq/pull/7439), an external contributor's draft that allows maintainer edits, with exactly this scope. Rust passes `fin_cancels: bool` through `poll_cancel`/`poll_withdrawn`/`until_withdrawn`/`resolve`, and `serve_track_status_stream` passes `false`. JS `RequestStream` takes `{ finCancels }`. Review it, mark it ready, and land it. Push the doc fix below onto it, or land it as a follow-up commit.
- #7423 asked that a TRACK_INFO behind a publisher refusing TRACK_STATUS fall back to subscribing:
  - ✅ Won't fix + doc fix: publishers must answer TRACK_STATUS. moqxr should implement it, which the maintainer called "really not hard". This matches the 2026-10-29 rejection of blind subscribing on [moq-dev/moq#7187](https://github.com/moq-dev/moq/pull/7187).
  - Fallback subscribe
  - Keep open, defer
- Doc: `doc/concept/standard.md:31` ends with "a reader whose publisher refuses `TRACK_STATUS` as `NOT_SUPPORTED` still subscribes". That is left over from the fallback that [moq-dev/moq#5991](https://github.com/moq-dev/moq/pull/5991) removed. Reword it so only a subscribing reader still subscribes, while a query-only or fetch-only reader fails, as `a_refused_track_status_never_subscribes` (`rs/moq-net/src/ietf/subscriber.rs`) pins.
- Not here: moqx's GROUP_ORDER parameter in TRACK_STATUS_OK, which draft 18 refuses. That is upstream moxygen's fix, facebookexperimental/moxygen#264, tracked by [Moxygen pin bump](/quest/m1/moxygen-pin-bump.md).

Public API: none. Wire: a draft-18 TRACK_STATUS is no longer withdrawn by a FIN alone. `release` refuses TRACK_STATUS, so nothing needs backporting.

## Closes

- [#7438](https://github.com/moq-dev/moq/issues/7438) - draft-18 TRACK_STATUS followed by a FIN is reset instead of answered

## Related

- [Moxygen pin bump](/quest/m1/moxygen-pin-bump.md) - the other half of playing through moqx on draft 18
