# [M] JS requests: one 10 s deadline from create to answer

## Goal

Every `@moq/net` request that waits for an answer (subscribe, standalone track
info, and fetch, on lite and moq-transport) runs under one fatal 10 s timer,
from creating its stream to the peer's first answer. When it fires, the
request fails, and any stream it created is reset, including one that opens
later. A create the transport refuses, or one still pending when the timer
fires, fails with a local "no stream credit" error; a request the peer opened
but never answered fails with `ControlTimeout`. Announce interest and
SubscribeNamespace bound their create the same way but have no answer
deadline, since they are long-lived.

Nothing retries either failure: waiting for stream credit is back-pressure,
and a timeout is terminal.

## Plan

Decided 2026-10-07, re-planning the external
[#4999](https://github.com/moq-dev/moq/pull/4999) and
[#5001](https://github.com/moq-dev/moq/pull/5001):

- Fail fast without credit. Per-request opens pass
  `waitUntilAvailable: false`. Chrome already rejects an over-limit create at
  once with a `NetworkError` (crbug.com/487117768), and stays terminal.
  Firefox ignores the flag and queues, so the one timer bounds it; a stream
  that opens after that is reset. `@moq/qmux` gets the same fix upstream.
- One timer, not two. The per-request opens drop `OPEN_TIMEOUT_MS`
  (`openWithin` in `js/net/src/stream.ts`), and the setup timers
  (`SUBSCRIBE_SETUP_TIMEOUT_MS` in lite, `SUBSCRIBE_OK_TIMEOUT_MS` in IETF)
  cover the create as well as the answer. SETUP, probe, and group streams keep
  their own bounds.
- The credit error is local, with no wire code. Propose its name in the PR;
  it is a new public `@moq/net` error.
- `@moq/watch` keeps ending a track on either failure. Rejected: #4999's
  resubscribe on a local timeout, since retrying adds a create per attempt and
  the timeout is meant to be fatal. Rejected: #5001's unbounded wait for a
  slot.
- Keep [#5002](https://github.com/moq-dev/moq/pull/5002)'s handling of a timer
  that fires mid-setup: the TRACK stream is reset, and a SUBSCRIBE stream that
  opens afterwards is reset with nothing written.
- Fix the stale text: "Chrome silently blocks" and "Chrome ~100" in both
  subscribers (the cap is the peer's MAX_STREAMS; the Rust relay grants
  10,000), `stream.ts`'s `QuotaExceededError` claim and its "matches the
  subscribe budget" note, and the "(browser stream limit reached?)" hint in the
  timeout messages.

Test with mocked time: a transport with a stream limit, covering an immediate
rejection, a queued create that times out with the credit error and is reset
when it opens, and an opened request with no answer that fails with
`ControlTimeout`. Cover lite and IETF draft 17.

Public API: one new error. Wire: none.

## Related

- [Rust fail fast](/quest/m1/rs-request-credit.md) - the same rule in moq-net
- [qmux no-wait opens](/quest/m1/qmux-no-wait.md) - the WebSocket fallback stops queueing over-limit creates
- [JS abandonment](/quest/m1/js-subscribe-abandonment.md) - edits the IETF setup path; land after its PRs to avoid conflicts
- [JS GOAWAY requests](/quest/m1/js-goaway-requests.md) - adds a check at every open site this touches
