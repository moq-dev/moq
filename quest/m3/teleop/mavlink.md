# [L] MAVLink bridge

## Goal

A `moq-mavlink` gateway replaces the VPN plus two unmanaged UDP flows that
every cellular ArduPilot vehicle runs today. QGroundControl, Mission Planner
and MAVProxy connect unchanged.

## Plan

### Shape

A library crate reached through `moq-cli`, the way `moq-srt` and `moq-rtmp`
already are (neither is a binary; both are crates behind subcommands).

Each side is bidirectional on one session. The vehicle side publishes what the
flight controller emits and subscribes the operator's commands to write back
into it; the operator side publishes commands and subscribes telemetry to serve
UDP 14550, because QGC speaks UDP and TCP only and its forwarding is one-way.
Neither side is a one-way media import or export, so do not let the subcommand
naming imply one leg.

### Route by msgid, never parse payloads

Read the header only: the 24-bit msgid in v2, 8-bit in v1. Payloads stay opaque
bytes and route to a delivery class through a config table, with an unknown
msgid falling to a default.

This is what makes frame-aware routing free of the dialect tax. CRC_EXTRA (the
per-message schema hash) is only needed to validate a frame, never to read its
header, so custom dialects, new messages and version skew all just work. The
alternative, compiling in `ardupilotmega` the way `mavlink-camera-manager` and
`mavlink-server` do, buys per-field logic we do not need and couples us to a
schema that changes without us.

### One lossy track for every msgid would starve the slow ones

Latest-wins is per track. A single lossy track carrying every routed msgid as
its own group lets 50 Hz `ATTITUDE` supersede a 1 Hz `HEARTBEAT` or a GPS fix,
because the newest group wins regardless of which message it holds. Putting
them all in one long-lived group has the opposite failure: nothing can be
skipped and head-of-line blocking is back.

So the lossy class is a latest-value snapshot, the shape `moq_json::snapshot`
implements, with the frame as an opaque value. Two details decide whether it
actually delivers latest-value:

- **Key on `(sysid, compid, msgid)`, not msgid alone.** A vehicle is several
  components, and an autopilot, a gimbal and a camera all emit `HEARTBEAT`
  under msgid 0; keying on msgid alone lets them overwrite each other. Decide
  explicitly what to do about instances distinguished only inside a payload,
  which the msgid-only rule cannot see.
- **Set the encoder's delta ratio to 0.** By default `moq_json::snapshot`
  batches up to `MAX_DELTA_FRAMES` merge patches into one ordered group
  (`rs/moq-json/src/snapshot/encoder.rs`), so under congestion an earlier 50 Hz
  attitude delta head-of-line blocks a later heartbeat inside that group. A
  ratio of 0 makes every change a self-contained snapshot, which is the
  latest-value contract; anything else has to justify the added delay.

`moq-flate` is the existing answer if the encoding overhead matters.

The reliable class is the append-log shape (`moq_json::stream`): commands,
ACKs, mission, parameter and file transfer are stop-and-wait exchanges carried
in one ordered group. Note the scope in
[robot](/quest/m3/teleop/robot.md): that is gap-free for a live reader, not
recoverable after a lag or a reconnect, so these MAVLink services keep relying
on their own retransmission across a link drop.

### Deliberately out of scope

Advertising the stream in-band. No `VIDEO_STREAM_TYPE` value can describe a
MoQ endpoint, so a GCS discovers the link out of band. Check the current
enumeration when this is picked up: it grew a WHEP entry after the original
four, which is the precedent that adding one upstream in `common.xml` is a
small, real contribution and its own future quest.

## Required

- [Robot teleoperation primitive](/quest/m3/teleop/robot.md)
- [Operator arbitration](/quest/m3/teleop/arbitration.md)
