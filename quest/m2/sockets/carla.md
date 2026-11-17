# [M] CARLA drivers publish their controls again

## Goal

The CARLA demo drives trucks without socket endpoints, in the format it had
before [moq-dev/moq#8011](https://github.com/moq-dev/moq/pull/8011). Each
driver publishes a broadcast at `viewer/carla/{truck}/{driver}` with a
`control` track; the worker follows the announcements under
`viewer/carla/{truck}/` for each truck and feeds every driver's controls
into the safety gate; acknowledgements ride the truck's shared `status`
track. The worker and the player need no version that carries hidden
endpoint broadcasts.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit): ✅ go back
to the old wire format (rejected: keeping the demo on sockets). The demo
itself stays in this repository because moq.dev hosts it (same date).

The old format, as `4d51f9c051fd^` (the parent of #8011) has it:

- **Paths** (`demo/carla/web/src/paths.ts`, `demo/carla/README.md`): cameras
  at `carla/{truck}/{camera}.hang`; one broadcast per driver at
  `viewer/carla/{truck}/{driverId}` with a `control` track of `moq-json`
  snapshots.
- **Controls**: `{"type":"drive","sequence":42,"steering":..}`, `estop`,
  `reset_estop`, and `respawn`, each with a `sequence` and `timestamps`.
- **Worker** (`demo/carla/sim/moq_carla_sim/publish.py`): an announce,
  follow, and request loop per truck. It lists `viewer/carla/{truck}/`,
  follows each driver path across that driver's runs, and pumps one run's
  `control` track at a time into the command queue, ending a run when
  another takes over or it goes offline.
- **Lease** (`drive.py`): bound to `(driver id, run counter)`. The counter
  is process-wide and local to the worker, from
  [moq-dev/moq#7678](https://github.com/moq-dev/moq/pull/7678): the bindings
  hand out a run's broadcast without its route instance, and the count keeps
  a run's late `driver_left` from releasing a later run of the same driver.
- **Status**: `driver`, `estop_by`, `sequence`, and `media_pipeline` on the
  truck's shared `status` track; the status is the control echo.
- **E-stop**: the holder's rides its control track; a bystander latches and
  resets through a short-lived `safety-<id>` broadcast without taking the
  lease.
- **Grants**: anonymous viewers publish under `demo/viewer/**`; the worker's
  token publishes `carla/**` and subscribes `viewer/carla/**`.

What #8011 changed and this undoes: `publish.Drivers` and the endpoint at
`carla/.server/{truck}`, the lease bound to a session id, replies on the
driver's session, the player's endpoint and its own origin, the
`VERSIONS` pin (`demo/carla/sim/moq_carla_sim/main.py:43`,
`demo/carla/web/src/paths.ts:39`), and the `demo/carla/.client/*` grant.

Restore the format, not the commit. For the PR:

- Keep what #8011's reviews fixed that is not about sockets (a bystander's
  e-stop failure surfaces; shutdown does not hang on a full queue), and the
  one-heartbeat (250 ms) delay budget on controls, so a reset right before a
  drive still arrives whole.
- Re-decide rather than restore, each with a recommendation:
  - The lease's run identity. Recommended: the local run counter returns as
    it was. The alternative, an `instance()` accessor on the bindings'
    broadcast consumer, was deferred on 2026-11-03 "if the move to sockets
    doesn't make that counter moot"; sockets are gone, so it is open again,
    and the bindings rule now adds an accessor only when a binding user
    needs it. The demo works without it.
  - The anonymous grant. Recommended: `demo/viewer/**` returns to
    `demo/relay/prod.toml` and the `packaging/moq-relay/relay.toml` sample,
    shared with MoQ Boy, whose viewers publish under `demo/viewer/` again
    ([moq-dev/moq#8936](https://github.com/moq-dev/moq/pull/8936)). Anyone who can publish
    under `viewer/carla/` can claim a driver id; the lease stays
    cooperative, and the README says so as it did.
  - The version list. Recommended: the worker and the player offer their
    defaults again.
  - Whether the player's element keeps the origin it took for the endpoint.
    Recommended: whichever is less code against today's `@moq/net`; the
    driver's control broadcast needs an origin to publish into either way.
- Update `demo/carla/README.md`, `doc/bin/demo.md` (the control bullet), the
  installer text under `demo/carla/sim/systemd/`, and the token example.

Tests: `just carla check` (the worker's lease, watchdog, and e-stop tests
against the restored loop) and `bun test` in `demo/carla/web`. The live
simulator needs the GPU image: drive a truck from a browser after deploying,
including a bystander e-stop, as #8011 asked.

Public API: none. Wire (app level): controls and acknowledgements move back
to broadcasts; the worker and the player deploy together.

## Related

- [moq.dev's hosted demos run the cut](/quest/m3/release/demos-deploy.md) - deploys this
- [moq-json writes take a value and a time](/quest/m2/json-write-at.md) - changes the write the `control` track uses
