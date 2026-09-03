# Teleoperation

## Goal

MoQ carries a robot's video down and its control up on one session, as a
library capability rather than a demo convention.

Generic in the library, integration-shaped in the proofs: Kyber is the first
transport replacement, while ArduPilot remains the first open protocol and
flight integration. The primitive is what any teleoperated machine needs. The
Kyber integration itself (replacing Kymux with moq-net and hang for Kyber's
media, input, feedback, and control) lives in Kyber's own repository, so it
has no quests here.

## Plan

### Why this is a library gap and not a demo

The pattern already works and is written down nowhere. `moq-boy` has each
viewer publish its own broadcast under a prefix, serve a JSON `command` track,
and receive per-stage glass-to-glass timestamps back on a `status` track; the
server walks the announce stream to fan the viewers in
(`rs/moq-boy/src/input.rs`). That is a teleoperation stack, discovered by one
demo and private to it. Every integrator rebuilds the announce fan-in, the
operator arbitration, and the latency instrumentation from scratch.

Two things are genuinely missing rather than merely undocumented: the hang
catalog is video plus audio only (location tracks arrived in moq#401 and were
dropped when the catalog became generic), and `moq-video` has no V4L2-M2M
encoder backend, so the boards that fly have no native hardware-encode path.

### Two delivery classes, one session

A robot on a cellular link is two unmanaged UDP flows today: RTP video to a
fixed port and MAVLink to another, competing over one bearer with nothing
prioritising control over video. Collapsing them onto one QUIC session is only
a win if the classes keep different delivery semantics: streamed telemetry and
manual control want the newest sample and nothing else, while commands,
mission, parameter and file transfer are stop-and-wait exchanges that must
arrive in order.

Reliable-by-default would lose. Peer-reviewed ROS 2 work finds reliable QoS on
a lossy link produces latency spikes rather than delivery, and an emulation
study on a long-RTT profile measured command staleness more than twice as bad
for QUIC reliable streams as for DDS best-effort: correct and useless.

The split is a framing decision, not a subscription flag, and `moq-json`
already implements both halves as its snapshot and stream modes. What that
means for the primitive is in [robot](/quest/m3/teleop/robot.md), and what it
means for a protocol multiplexing many message rates onto one link is in
[mavlink](/quest/m3/teleop/mavlink.md).

### Who is already here

Nobody runs DDS over a WAN. The fight is MoQ against Zenoh over QUIC and MoQ
against Foxglove on WebRTC.

- **Zenoh** shipped priority-mapped QUIC multistream plus mixed
  stream/datagram reliability in v1.9.0 (April 2026) and is a Tier 1 ROS 2
  middleware. Closest thing to our delivery model in the wild, but its WAN
  topology is hand-configured router endpoints with no relay or media pipeline.
- **Foxglove** Remote Access went GA in August 2026 as a hand-rolled MoQ: the
  device gateway connects outbound, uploads each stream at most once to an SFU
  that fans out, uses lossy data channels by default with reliable opt-in per
  topic, and adapts video quality. Built on WebRTC because nothing else existed.
- **Kyber** (kyber.tech, Jean-Baptiste Kempf of VLC, $5M seed June 2026) is the
  only other party betting on QUIC over WebRTC for machine control. Point to
  point with no relay or fan-out story, proprietary framing with no interop, no
  ROS or MAVLink integration, at v0.26. The competition is over the narrative,
  not the technology.

### CGNAT is the pain the ecosystem routes around

Cellular vehicles sit behind carrier NAT, and the ecosystem's answer is
ZeroTier or Tailscale. A client-initiated QUIC session to a relay removes the
VPN, the competing UDP flows, and the signalling server at once, and survives
cell handover through connection migration. That is the pitch, and it is worth
stating plainly because it is what a builder is comparing against.

## Quests

- [Robot teleoperation primitive](/quest/m3/teleop/robot.md) - a `moq-robot`
  crate carrying the track shapes and discovery every teleoperated machine
  needs; gates the rest
- [Operator arbitration](/quest/m3/teleop/arbitration.md) - exactly one
  controller commands a vehicle at a time, with explicit handoff and a stated
  authorization boundary
- [MAVLink bridge](/quest/m3/teleop/mavlink.md) - a `moq-mavlink` gateway
  replacing the VPN plus two unmanaged UDP flows, with QGroundControl and
  friends unchanged
- [Browser teleoperation package](/quest/m3/teleop/browser-package.md) - `@moq/robot`
  mirrors the Rust crate, so browser clients consume the catalog and delivery
  classes
- [SITL proof and browser ground station](/quest/m3/teleop/proof.md) - ArduPilot
  SITL and a synthetic camera flown from a browser ground station, reproducible
  in five minutes
- [V4L2-M2M encoding](/quest/m3/teleop/v4l2-encode.md) - `moq-video` encodes in
  hardware on the boards that fly, so `moq-cli` needs no GStreamer detour on a
  Raspberry Pi
- [Teleoperation use-case docs](/quest/m3/teleop/docs.md) - `doc/concept/use-case/other.md`
  becomes the teleoperation page, with a runnable non-media example beside it
- [ROS 2 bridge](/quest/m3/teleop/ros2.md) - a ROS 2 bridge sibling to the
  MAVLink one, carrying topics over the same two delivery classes
- [Cross-track correlation](/quest/m3/teleop/correlation.md) - a command, the
  telemetry it produced, and the video frame showing the result share one
  timebase
- [Validate VAAPI encoding](/quest/m3/teleop/vaapi.md) - prove `moq-video`'s
  VAAPI backend on real hardware so it stops being opt-in

## Related

- [e2ee](/quest/m2/e2ee/README.md) - the answer for a protected control link
- [Text schema](/quest/m2/text-schema.md) - non-media tracks in a catalog,
  arrived at from the media side
- [Publisher-reported media stats](/quest/m2/qos/publisher-stats.md) -
  publisher-reported stats as a catalog section (moq#2734); teleop's latency
  instrumentation extends that track rather than adding a second one
