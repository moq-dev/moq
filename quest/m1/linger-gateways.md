# [L] Gateway imports linger at a clean end

## Goal

The CLI's gateway imports (HLS, RTMP, SRT, RTC) finish their broadcast when
the source ends cleanly, so subscribers still catching up drain the tail
until the media is stale. An abrupt end (a dropped connection, an error)
still closes at once, and so does an interrupt.

A newer epoch published at the same path cuts the old instance's linger
short, so an encoder that reconnects does not leave the finished instance
announced next to its replacement.

## Plan

Decided (2026-10-16):

- Publishers call `broadcast::Producer::finish()`, which returns a
  `broadcast::Finish` handle, and await `Finish::wait()`; dropping the
  handle is the interrupt. Tracks without a max-age add nothing to the
  wait. `ts::Programs::finish` already returns a `Finish` for every
  program; `moq-srt` drops them today, keeping its close-now behavior.
- A newer epoch cuts the old linger short. Requests on a replaced instance
  already end with an error, because instances are never stitched, so the
  old instance has nothing left to serve once replaced. Recommended: put the
  cut where every gateway gets it at once, rather than in each gateway.

Define a clean end per protocol (such as an RTMP `deleteStream`, an SRT or RTC
close after the final media, an HLS playlist that ends). HLS import never
ends cleanly today: its loop runs until cancelled, so it needs a clean end
first.

Test with a mocked clock: a clean end lingers for max-age, an abrupt end
closes at once, and a reconnect with a newer epoch ends the old linger.
