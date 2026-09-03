# [M] SITL proof and browser ground station

## Goal

ArduPilot SITL plus a synthetic camera, flown from a browser ground station
through a real relay, with no hardware. Someone with a laptop reproduces it in
five minutes.

## Plan

The browser client subscribes video and telemetry and publishes control, the
shape Blue Robotics' Cockpit proves is viable. It is the demo and the
end-to-end proof of the primitive, not a bid to out-feature QGroundControl:
real users reach the same link through `moq export mavlink`.

SITL is the right proof surface precisely because it removes the camera, which
otherwise dominates the latency budget and would make the demo a measurement of
somebody's webcam.

Standing SITL up in CI is separate work with its own build dependencies.
Reproducibility by hand is the bar here; automate it later if it proves worth
the maintenance.

## Required

- [MAVLink bridge](/quest/m3/teleop/mavlink.md)
- [Browser teleoperation package](/quest/m3/teleop/browser-package.md)
