# [M] SITL proof and browser ground station

## Goal

ArduPilot SITL plus a synthetic camera, flown from a browser ground station
through a real relay, with no hardware. Someone with a laptop reproduces it in
five minutes.

## Plan

Parked in m3 until a real ArduPilot user or partner asks (2026-09-30 audit).

The browser client subscribes video and telemetry and publishes control, the
shape Blue Robotics' Cockpit proves is viable. It is the demo and the
end-to-end proof of the primitive, not a bid to out-feature QGroundControl:
real users reach the same link through the `moq-mavlink` gateway.

SITL is the right proof surface precisely because it removes the camera, which
otherwise dominates the latency budget and would make the demo a measurement of
somebody's webcam.

The ground station's catalog and delivery-class shapes come from a zod
schema mirroring the `moq-robot` types, written once and extending the root
schema through the same seam `js/hang/src/catalog/root.ts` uses (there is no
`@moq/mux`). Decided 2026-10-08: no separate `@moq/robot` package until a
second browser consumer needs it; the schema folds in here. A browser
observer cannot degrade the operator's classes (the publisher's
`Info::max_age` bounds the window), so the schema is for reuse, not safety.

Standing SITL up in CI is separate work with its own build dependencies.
Reproducibility by hand is the bar here; automate it later if it proves worth
the maintenance.

## Required

- [MAVLink bridge](/quest/m3/teleop-mavlink.md) - the gateway the vehicle side runs

## Related

- [Robot teleoperation primitive](/quest/m3/teleop-robot.md) - the Rust types the zod schema mirrors
