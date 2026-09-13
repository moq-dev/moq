# [M] Cross-track correlation

## Goal

A command, the telemetry sample it produced, and the video frame showing the
result share one timebase, so a teleoperation recording is usable as training
data and an operator can be shown what the machine actually saw.

## Plan

The two hosts' clocks are related by deployment assumption, not by the library:
a robot deployment already synchronizes both ends. Each broadcast's fixed
catalog-root `clock: { wall, timescale }` is the bridge. Media and command
tracks keep their own timescales; convert PTS explicitly into the broadcast
clock before joining samples. The robot's video and telemetry share a clock;
the operator's command broadcast supplies its own synchronized mapping.

Source restarts preserve each broadcast's mapping. There are no per-record
anchors or mutable `set_wall` epochs. Report whether a mapping is present,
but never infer that hosts are synchronized from its presence. State the clock
assumption beside the API, since a join across unsynchronized hosts can look
valid while being wrong. The library provides no clock-sync mechanism; see
[#2278](/quest/m2/2278-watch-absolute-wall-clock-latency-target-for-synchronized.md).

This is also the answer to Kyber's headline claim of continuous drift
computation onto one unified timeline. Worth answering on the merits:
correlating sensor, command and video is the actual product need, and it is
the same property that makes an MCAP recording valuable.

## Required

- [Robot teleoperation primitive](/quest/m3/teleop/robot.md)
- [Publisher clocks](/quest/m2/publisher-clock.md) - publishers populate the fixed broadcast mapping used to join tracks
