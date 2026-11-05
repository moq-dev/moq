# [S] The browser viewer follows a fallback without dropping its session

## Goal

When the newest of two live publishers withdraws, a browser `@moq/watch`
viewer on moq-transport-22 or moq-lite keeps its session and plays the
still-live older run within the media lane's derived 1039 ms bound. The
session drop is fixed at its cause once it recurs with enough evidence to
reproduce it deterministically.

## Waiting

- moq-dev/moq: the session drop recurs with relay debug logs and the viewer's
  close reason. It has not recurred since
  [moq-dev/moq#6548](https://github.com/moq-dev/moq/pull/6548)'s IETF22 fixes;
  clean runs do not clear this wait. Advance it by running `just test media
  --media-cases replacement --media-engines chromium` with
  `RUST_LOG=moq_net=debug,moq_relay=debug` (inside the dev shell, under
  `env -u MOQ_QUEST`).

## Plan

**Session drop:** seen 1 in 9 runs before
[moq-dev/moq#6548](https://github.com/moq-dev/moq/pull/6548)'s IETF22 fixes
(host load average 30 to 70), 0 in 37 since. After the fallback NAMESPACE, the
viewer restarted its subscriptions. Then the relay stopped its own
SUBSCRIBE_NAMESPACE stream to the viewer, and the session closed. The viewer
reconnected and played the older run, but only after the bound. Relay debug
logging was off for that run, so it cannot say whether the relay closed over a
violation or the session closed cleanly. The viewer's reconnect warning now
names the close reason; start there.

The audio stall that shared this quest was fixed in
[moq-dev/moq#7656](https://github.com/moq-dev/moq/pull/7656): on-demand audio
encoders replayed all PCM retained while unsubscribed, ignoring the demand's
max delay, so a rejoining live viewer drained seconds of stale groups before
audio resumed.

Public API: none expected. Wire: none expected.
