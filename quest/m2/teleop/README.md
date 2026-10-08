# Teleoperation

## Goal

MoQ carries a robot's video down and its control up on one session, as a
library capability rather than a demo convention.

Generic in the library: the primitive is what any teleoperated machine needs,
and integrations are adapters on top of it. ArduPilot is the first open
protocol and flight integration, parked in m3 until a real ArduPilot user or
partner shows up.

Decided in the 2026-09-30 audit: the ROS 2 bridge was deleted, cross-track
correlation folded into [robot](/quest/m3/teleop-robot.md), V4L2-M2M encoding
folded into [CLI packaging](/quest/m2/cli-packaging.md), and the MAVLink
bridge and SITL proof moved to m3. Kyber is a competitor with proprietary
framing, not a transport we replace.

Decided 2026-10-08: the robot primitive and operator arbitration are parked in
m3 until a teleoperation consumer needs them, so the line's m2 work is the
use-case docs. The browser package was deleted; its shared schema folds into
the [SITL proof](/quest/m3/teleop-proof.md).

## Plan

### Why this is a library gap and not a demo

The pattern already works and is written down nowhere. `moq-boy` has each
viewer publish its own broadcast under a prefix, serve a JSON `command` track,
and receive per-stage glass-to-glass timestamps back on a `status` track; the
server walks the announce stream to fan the viewers in
(`rs/moq-boy/src/input.rs`). That is a teleoperation stack, discovered by one
demo and private to it. Every integrator rebuilds the announce fan-in, the
operator arbitration, and the latency instrumentation from scratch.

The hang catalog is no longer a gap: it advertises data tracks in its `json`
and `binary` sections beside video and audio. The other missing piece,
`moq-video`'s V4L2-M2M encoder in a released `moq-cli`, is tracked by
[CLI packaging](/quest/m2/cli-packaging.md).

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

The split is a framing decision, not a subscription flag, and `moq-json` and
`moq-flate` already implement both halves as their snapshot and stream
modes. What that
means for the primitive is in [robot](/quest/m3/teleop-robot.md), and what it
means for a protocol multiplexing many message rates onto one link is in
[mavlink](/quest/m3/teleop-mavlink.md).

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

## Required

- [Teleoperation use-case docs](/quest/m2/teleop/docs.md) - `doc/concept/use-case/`
  gains a teleoperation page, with a runnable non-media example beside it

## Related

- [Robot teleoperation primitive](/quest/m3/teleop-robot.md) - a `moq-robot`
  crate carrying the track shapes and discovery every teleoperated machine
  needs, parked in m3
- [Operator arbitration](/quest/m3/teleop-arbitration.md) - exactly one
  controller commands a vehicle at a time, with explicit handoff and a stated
  authorization boundary, parked in m3
- [e2ee](/quest/m1/e2ee/README.md) - the answer for a protected control link
- [Media stats](/quest/m1/stats/schema.md) - publisher-reported stats
  on a catalog-announced track (moq#2734); teleop's latency instrumentation
  extends those types rather than adding a second stats surface
- [Video hardware validation](/quest/m3/video-hardware.md) - the VAAPI run
  that covers Intel ground robots and NUC companions
- [CLI packaging](/quest/m2/cli-packaging.md) - ships the V4L2-M2M encoder the
  boards that fly need
- [MAVLink bridge](/quest/m3/teleop-mavlink.md) - a `moq-mavlink` gateway,
  parked until a real ArduPilot user or partner
- [SITL proof and browser ground station](/quest/m3/teleop-proof.md) - ArduPilot
  SITL flown from a browser, parked with the bridge
