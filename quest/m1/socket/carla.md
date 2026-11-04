# [M] CARLA on sockets

## Goal

Each CARLA truck accepts drivers through a `Listener` at `carla/<truck>`, and
the web driver dials it, so `demo/carla/sim` and `demo/carla/web` no longer
watch `viewer/carla/<truck>/` or request driver broadcasts by hand. Each
driver's ack and sequence echo come back on its own socket. The one-driver
lease, takeover, e-stop, the 1 s watchdog, and handback behave as before.

## Plan

See the [epic](/quest/m1/socket/README.md) for the layout and the reply split.

- The worker (`publish.py`) uses the Python `Listener`; `paths.ts` and
  `truck.ts` dial `carla/<truck>` with the per-lease driver id. A new lease
  is a new epoch and so a new socket, which matches today's lease binding.
- The `control` track moves onto the socket. The driver's ack, sequence, and
  round-trip echo move from the shared `status` to the driver's `.accept`
  broadcast. `driver` and `estop_by` stay in `status`, because every viewer
  shows them.
- Decide in the PR whether the `safety-<id>` e-stop broadcast becomes its own
  socket or a second track on the driver's socket, and record why.
- The anonymous publish grant covers `demo/carla/*/.dial/*`. Whichever of
  this and [moq-boy on sockets](/quest/m1/socket/boy.md) lands second removes
  `demo/viewer/**` from `demo/relay/prod.toml` and its documented
  `moq auth serve` line. The worker's token keeps publish on `carla/**`
  and swaps its subscribe scope from `viewer/carla/**` to `carla/*/.dial/*`,
  because publish and subscribe are granted separately; say so in
  `demo/carla/README.md`.
- Update `demo/carla/README.md` (paths, lease) and `doc/bin/demo.md`.
- Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs): [moq-dev/moq#7678](https://github.com/moq-dev/moq/pull/7678) keys carla's
  Python driver lease on a local run counter, because moq-ffi's
  `MoqBroadcastConsumer` exposes no instance. If the move to sockets doesn't
  make that counter moot, add a `MoqBroadcastConsumer::instance()` accessor
  (and its Python wrapper) and key the lease on the real instance. Options
  shown: m0; ✅ m1, in this quest; m2.

## Required

- [Python sockets](/quest/m1/socket/py.md) - the truck's listener
- [JS sockets](/quest/m1/socket/js.md) - the driver's dial
