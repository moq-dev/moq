# [M] CARLA on sockets

## Goal

Each CARLA truck runs a socket endpoint named for the truck and accepts
drivers on it, and the web driver dials it from its own endpoint, so
`demo/carla/sim` and `demo/carla/web` no longer watch `viewer/carla/<truck>/`
or request driver broadcasts by hand. Each driver's control, ack, sequence
echo, and e-stop travel on its own sessions. The one-driver lease, takeover,
e-stop, the 1 s watchdog, and handback behave as before.

## Plan

See the [epic](/quest/m1/socket/README.md) for the convention.

- The worker (`publish.py`) runs a Python endpoint per truck; `paths.ts` and
  `truck.ts` dial it from an endpoint with the per-lease driver id. A new lease
  is a new session, which matches today's lease binding.
- The `control` track becomes the driver's messages on a `track` session, one
  control per group, so a late control never blocks a newer one; the driver
  may read the truck's replies with a finite delay budget. The
  driver's ack, sequence, and round-trip echo move from the shared `status` to
  the truck's messages on that session. `driver` and `estop_by` stay in the
  ordinary `status` broadcast, because every viewer shows them.
- The `safety-<id>` e-stop broadcast becomes messages on the driver's session
  or a second session of its own; decide in the PR and record why, such as
  keeping the e-stop off the control session's queue.
- The anonymous grants let a driver publish its own endpoint and subscribe to
  the truck endpoints. Whichever of this and
  [moq-boy on sockets](/quest/m1/socket/boy.md) lands second removes
  `demo/viewer/**` from `demo/relay/prod.toml` and its documented
  `moq auth serve` line. The worker's token keeps publish on `carla/**`, which
  covers its endpoints, and swaps its subscribe scope from `viewer/carla/**`
  to the driver endpoints; say so in `demo/carla/README.md`.
- Update `demo/carla/README.md` (paths, lease) and `doc/bin/demo.md`.

## Required

- [Python sockets](/quest/m1/socket/py.md) - the truck's endpoint
- [JS sockets](/quest/m1/socket/js.md) - the driver's dial
