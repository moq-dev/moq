# [XL] Every crate on moq-clock

## Goal

Every remaining workspace crate (moq-tokio, moq-relay, moq-cli,
moq-hls, moq-auth, moq-archive, moq-audio, moq-video, moq-gst, moq-rtc,
moq-rtmp, moq-srt, moq-shaper, moq-sock, moq-transcode, moq-e2ee, moq-boy,
moq-bench, moq-c, hang, and the rest under `rs/`) reads time and arms timers
only through `moq-clock`, and tests needed to prove those migrations use the sim, paused tokio, or
events. No production direct `Instant::now`, `SystemTime::now`, `tokio::time`, or
`std::thread::sleep` remains outside moq-clock's backends and explicit
performance measurement (moq-bench's timings).

## Plan

Follow the real-I/O rule in [the line's README](/quest/m1/time/README.md):
a migrated test with no real socket, process, or device uses the sim (or paused
tokio until it can); one that needs real I/O waits on events, never on paused
tokio, which auto-advances to a timer before the OS event arrives, and never
on a fixed sleep. moq-relay's integration tests move onto the sim in
[the relay seam](/quest/m1/time/relay-seam.md), not here.

The targeted moq-auth grant conversion belongs to
[auth refresh](/quest/m1/test-flakes-2/auth-refresh-mock-time.md); reuse that
change rather than implementing a second migration. It need not wait here.

Fix mixed clocks first: anything that reads std time but waits on tokio time
(a backoff's stability window, a gate computed with std `Instant` and slept
with tokio) is a bug under paused time.

Split the PR by crate group if review needs it; ban direct clock reads in each
production scope as it finishes. Unrelated legacy test conversions and the
workspace-wide lint ratchet follow in m2.

Public API: breaking wherever a crate exposes an instant or a time source.
Wire: none.

## Required

- [moq-net on moq-clock](/quest/m1/time/net.md) - these crates consume moq-net's instant

## Related

- [Time test sweep](/quest/m2/time-test-sweep.md) - unrelated test-only/private clock cleanup and global lint are deferred; migrate production/public APIs and their required regression tests here
