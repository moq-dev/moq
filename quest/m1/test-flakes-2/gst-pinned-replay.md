# [S] The moq-gst pinned-instance test waits on events

## Goal

`a_pinned_instance_winning_its_path_back_does_not_replay_its_finish`
(`rs/moq-gst/src/source/imp.rs:3960`) never fails under load.

## Plan

It drives a real GStreamer element against a real-time origin and polls
with `std::thread::sleep(50ms)` loops capped at 200 iterations, racing the
pipeline threads. Wait on the announce and pad events themselves, and drive
the origin's timers with a `moq_clock::Manual` clock.

Public API: none. Wire: none.
