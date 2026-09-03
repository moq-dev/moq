# [M] Cross-track correlation

## Goal

A command, the telemetry sample it produced, and the video frame showing the
result share one timebase, so a teleoperation recording is usable as training
data and an operator can be shown what the machine actually saw.

## Plan

The hard part is time transfer between two hosts, and it is the part with no
existing answer. `rs/moq-mux/src/clock.rs` is a process-local `Instant` epoch
for relating capture threads inside one process; it has no wall anchor and
cannot relate an operator's command timestamp to a robot's frame timestamp. The
wall anchor is `timeline.rs`'s `Producer::set_wall`.

So the open question is how the two clocks are related: assume NTP on both
ends, derive an offset from session RTT, or stamp only at the robot and have
the operator echo its stamps back the way `moq-boy`'s `status` track already
does. Settle that before designing the contract; the third option needs no
external dependency and is the one already proven in-tree.

This is also the answer to Kyber's headline claim of continuous drift
computation onto one unified timeline. Worth answering on the merits:
correlating sensor, command and video is the actual product need, and it is the
same property that makes an MCAP recording valuable.

## Required

- [Robot teleoperation primitive](/quest/m3/teleop/robot.md)
