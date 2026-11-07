# [XS] moq-boy pause state

## Goal

A viewer who subscribes to a paused moq-boy session always wakes the
emulator. No interleaving of the pause monitors and the emulator thread
leaves it asleep while a track is wanted. Pause state is one value derived
from which tracks are wanted, written only under the lock the emulator
waits on.

## Plan

The shared decisions are in the [epic](/quest/m1/explicit-state/README.md).

The bug (`rs/moq-boy/src/main.rs`, `Session`):

- `video_active`, `audio_active`, and `paused` are separate atomics beside a
  `(Mutex<()>, Condvar)`.
- `run_track_monitor` and `run_pause_monitor` store `paused = false` and call
  `notify_all()` without taking the mutex, while `wait_for_resume` checks
  `paused` under the mutex and then waits.
- A notify that lands between that check and `cvar.wait` is lost, so the
  emulator sleeps with `paused == false` until the next subscription change.
- Separately, `run_pause_monitor`'s `tokio::join!` of both "unwanted" waits
  doesn't re-check a side once it resolves, so it can store `paused = true`
  while video is wanted again.

Recommended shape: the condvar's own mutex holds the demand
(`{ video: bool, audio: bool }`), and paused means neither is wanted. Each
monitor changes the demand under the lock and then notifies. That also lets
the two monitors collapse into one.

[moq-dev/moq#8115](https://github.com/moq-dev/moq/pull/8115) (socket roles)
also edits this file. Expect a merge conflict, not an overlap.
