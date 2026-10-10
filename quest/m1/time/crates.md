# [XL] Every crate on moq-time

## Goal

Every remaining workspace crate (moq-tokio, moq-relay, moq-cli,
moq-hls, moq-auth, moq-archive, moq-audio, moq-video, moq-gst, moq-rtc,
moq-rtmp, moq-srt, moq-shaper, moq-sock, moq-transcode, moq-e2ee, moq-boy,
moq-bench, moq-c, hang, and the rest under `rs/`) reads time and arms timers
only through `moq-time`, and its tests run on the sim, paused tokio, or events. No
direct `Instant::now`, `SystemTime::now`, `tokio::time`, or
`std::thread::sleep` remains outside moq-time's backends and explicit
performance measurement (moq-bench's timings).

## Plan

Follow the real-I/O rule in [the line's README](/quest/m1/time/README.md):
a test with no real socket, process, or device moves to the sim (or paused
tokio until it can); one that needs real I/O waits on events, never on paused
tokio, which auto-advances to a timer before the OS event arrives, and never
on a fixed sleep. moq-relay's integration tests move onto the sim in
[the relay seam](/quest/m1/time/relay-seam.md), not here.

Fix mixed clocks first: anything that reads std time but waits on tokio time
(a backoff's stability window, a gate computed with std `Instant` and slept
with tokio) is a bug under paused time.

Split the PR by crate group if review needs it; ban direct clock reads in each
crate as it finishes (the ratchet).

Public API: breaking wherever a crate exposes an instant or a time source.
Wire: none.

## Required

- [moq-net on moq-time](/quest/m1/time/net.md) - these crates consume moq-net's instant
