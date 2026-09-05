# [M] Cross-track correlation

## Goal

A command, the telemetry sample it produced, and the video frame showing the
result share one timebase, so a teleoperation recording is usable as training
data and an operator can be shown what the machine actually saw.

## Plan

The two hosts' clocks are related by assumption, not by the library: a robot
deployment already runs NTP or PTP on both ends, and the `moq-robot` contract
says so. Each track's `Timeline.wall` anchor is the bridge, read as data from
the catalog; the library offers no clock-sync mechanism and the crate must
not add one (see [#2278](/quest/m2/2278-watch-absolute-wall-clock-latency-target-for-synchronized.md)
for the decision). `rs/moq-mux/src/clock.rs` is a process-local `Instant`
epoch for relating capture threads inside one process and cannot relate an
operator's command to a robot's frame; `timeline.rs`'s `set_wall` is what can.

So the contract is: the robot anchors its video and telemetry timelines, the
operator anchors its command track, and a recording joins the three on
`wall + pts`, resolving each sample against the latest anchor at or before it
(the catalog `wall` until a timeline record after a discontinuity carries a
new one). State the clock assumption beside the API. Report only whether an
anchor is present; presence says nothing about whether the hosts are synced,
and a join across unsynced hosts looks valid while being wrong, so synchronization
is a declared deployment property, never inferred from the data.

This is also the answer to Kyber's headline claim of continuous drift
computation onto one unified timeline. Worth answering on the merits:
correlating sensor, command and video is the actual product need, and it is
the same property that makes an MCAP recording valuable.

## Required

- [Robot teleoperation primitive](/quest/m3/teleop/robot.md)
- [Publishers anchor the timeline](/quest/m2/timeline-wall.md) - the anchor every track needs before it can be joined
