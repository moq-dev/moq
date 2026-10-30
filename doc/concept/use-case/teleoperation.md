---
title: MoQ for Teleoperation
description: Carry video, current control state, and ordered commands on one session
---

# MoQ for Teleoperation

A remote operator needs fresh video from a machine and a responsive control path
back to it. MoQ carries both directions over one session to a relay. The machine
connects outbound, publishes its video and telemetry, and subscribes to the
operator's control broadcast. Additional viewers subscribe through the relay
without another upload from the machine.

A VPN plus separate video and control UDP flows can provide reachability, but the
application still has to coordinate their delivery and congestion. With MoQ,
tracks sent in the same direction share a connection and its congestion budget,
so the machine can rank telemetry above its video. Priority only orders data
queued at one sender: the operator's commands travel the other way, so they never
compete with the machine's video, and priority does not guarantee a deadline or
make a command safe to execute. Keep each kind of data on a track with the
framing it needs.

## Choose the delivery shape

| Data | Shape | What falling behind means |
| --- | --- | --- |
| Steering, throttle, current position, sensor state | JSON snapshots | Catch up to the newest state; intermediate values are superseded. |
| Arm, stop, a parameter change, mission or file records | Ordered JSON stream | Consume every record in order, or report an error. |
| Camera and microphone | Media groups | Join at a decodable boundary and choose the playback buffer. |

Use [`moq-json`](https://docs.rs/moq-json) snapshot mode for replaceable state. It
starts a group with a full value and carries changes as deltas. A reader can skip
to a newer group and reconstruct the current value. Stream mode puts an ordered
log in one group; it reports a broken or rolled stream instead of silently
presenting a gap as a complete log. In Rust that group grows for the track's
life, but the cache keeps only its newest 32 MiB, so a reader that falls further
behind or joins late fails with `Evicted`; `@moq/json` still bounds it at 8,192
records. Either way, open a new command track per control session rather than
replaying one forever. These are framing choices, not a universal
'reliable' switch on the session.

Transport delivery is not command execution. An application that needs to know
whether a command ran sends a command ID and an acknowledgment, defines duplicate
handling, and rejects expired control state at the machine. It also decides who
may control the machine. A subscriber being connected does not grant operator
ownership.

## Run a non-media example

From the repository root:

```bash
nix develop --command cargo run -p moq-json --example teleop
```

The [example source](https://github.com/moq-dev/moq/blob/main/rs/moq-json/examples/teleop.rs)
creates `control.json` and `commands.json` tracks in one in-process broadcast.
It writes several control states before reading and gets only the latest one;
it reads all three commands in order. It uses fixed media timestamps and needs
no network service or robot.

```text
Current control: Control { speed: 0, turn: 0 }
Command: Arm
Command: Stop
Command: Disarm
```

To send these tracks over a network, publish the broadcast through a
[`moq-tokio` origin](https://docs.rs/moq-tokio/latest/moq_tokio/origin/index.html)
and connect that origin to a [relay](/bin/relay/). Publish the machine's media
and telemetry on its broadcast and the operator's control on another. Hang's
[catalog](/concept/hang) advertises JSON and binary tracks beside audio and video
when an application needs to discover them together. Give each machine and
operator the [token permissions](/bin/relay/auth) for their own publish and
subscribe paths.
