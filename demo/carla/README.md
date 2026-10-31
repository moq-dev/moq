# CARLA teleop demo

Drive simulated trucks from a browser over MoQ. Trucks share one
[CARLA](https://carla.org) world and drive themselves until someone takes
control. Each truck publishes its cameras and a status track; a web player
watches them on a map and publishes a steering **control** track back. Control
follows the [moq-boy](../boy) pattern: the viewer publishes, the truck
subscribes, and the truck echoes what it accepted on `status`.

```text
├── web/      # the player: the <moq-carla> element, with map, video, keyboard and steering-wheel input (@moq/demo-carla)
├── sim/      # the worker: one Python process driving every truck in CARLA
│   ├── moq_carla_sim/  # safety gate, CARLA world, MoQ publish and control
│   └── systemd/        # rootless user units for an always-on workstation
├── bridge/   # Rust: the GPU video bridge the simulator loads (moq-carla-bridge)
└── justfile  # `just carla <recipe>`
```

The simulator itself is a CARLA source build with the bridge's Unreal module,
in [moq-dev/carla](https://github.com/moq-dev/carla).

## Running locally

Needs a Linux host with an NVIDIA GPU and podman; the repo's dev shell
(`nix develop`, or direnv) provides everything else. Build the simulator image
with [moq-dev/carla](https://github.com/moq-dev/carla) and the worker's
virtualenv once, then leave the simulator running:

```bash
git clone https://github.com/moq-dev/carla ../carla
../carla/build.sh .                   # from this checkout's root; prints the image tag
just carla venv
CARLA_IMAGE=<tag> just carla server   # separate terminal
just carla                            # relay + every truck + web player
```

`just carla cdi` generates the rootless `nvidia.com/gpu` CDI device the
container runs with; rerun it after a driver update, and `just carla server`
refuses a stale one. On a SELinux host the container runs with
`label=disable`, because SELinux denies `/dev/nvidia*` in a way that looks like
a missing driver.

The player alone runs against any relay with `just carla web <url>`. Append
`?preview` to the local URL for a fixture-driven layout review that opens no
connection; it is removed from production builds.

## Broadcasts

```text
carla/{truck}/{camera}.hang       <- one media broadcast per camera (hang catalog)
  sd                                 240p H.264, for map previews
  hd                                 720p H.264, for the focused view
  status                             JSON Snapshot telemetry

viewer/carla/{truck}/{driverId}   <- one broadcast per driver
  control                            JSON Snapshot commands
```

The worker consumes `viewer/carla/{truck}/` for each truck, fanning in every
driver's `control` track. Cameras are sibling broadcasts, so switching views
never creates a second claim on the same wheel, and the driver scope is
disjoint from every media path.

### Control (player to truck)

```json
{ "type": "drive", "sequence": 42, "steering": -1.0..1.0,
  "throttle": 0..1, "brake": 0..1,
  "gear": "neutral|forward|reverse", "timestamps": [...] }
{ "type": "estop", "sequence": 43, "timestamps": [...] }
{ "type": "reset_estop", "sequence": 44, "timestamps": [...] }
{ "type": "respawn", "sequence": 45, "timestamps": [...] }
```

### Status (truck to player)

```json
{ "accepted": { "steering": .., "throttle": .., "brake": .., "gear": ".." },
  "autonomy": "autonomous|manual", "estopped": true,
  "driver": "ab12cd", "estop_by": "ef34gh", "sequence": 44,
  "media_pipeline": { "ab12cd": [ { "label": "received→decoded", "ms": 12 } ] },
  "map": "Town10HD_Opt",
  "pose": { "x": 12.35, "y": -78.9, "yaw": 180.1, "speed": 9.88 },
  "measured": { "steering": .., "throttle": .., "brake": .., "gear": ".." },
  "motion": { "yaw_rate": 3.4, "accel_lon": 1.23, "accel_lat": -2.35 },
  "collision": { "at": 1756334400123, "other": "vehicle.tesla.model3", "impulse": 4210.6 } }
```

Status republishes every 100 ms whether or not anyone is driving, since an
autonomous truck changes no accepted state as it moves. `pose` is in CARLA
world metres (x east, y south, yaw clockwise from +x), which is what the
player's baked road maps use.

`accepted` is the gate repeating what it let through. `measured`, `motion`, and
`collision` come from the vehicle itself, so the player can draw requested
against measured steering and show the gap. `collision.at` is the unix
millisecond contact began: a fixed instant gives the event an identity on a
snapshot track, so a late joiner never mistakes a retained collision for a
fresh one. Continuing contact with the same actor within a second is one
collision.

### The safety gate

The worker's gate (`sim/moq_carla_sim/drive.py`) owns every truck's controls:

- **One driver at a time.** The first driver to send a command holds the
  lease until its broadcast ends or its 1 s watchdog expires; the player
  heartbeats every 250 ms. The lease is bound to the driver's path and the
  epoch it announced with, so a driver that reconnects at the same path is a
  new claimant rather than a continuation.
- **E-stop latches** until an explicit reset, from any viewer: the player
  latches and resets it through a short-lived safety broadcast without taking
  the lease.
- **Gear changes are interlocked**: the gate keeps the current gear unless the
  command releases throttle and applies some brake.
- **An unclaimed truck goes back to autonomy**, not to full brake. It first
  coasts onto the nearest lane running its way (`handback.py`), because
  Traffic Manager steers a truck it takes back toward the nearest waypoint
  whatever its heading.
- **A wedged truck respawns**: an autonomous truck that has not moved 2 m in
  5 minutes returns to a free spawn point.

The lease is cooperative. Anyone who can publish under `viewer/carla/` can
announce a driver path, so it stops two honest drivers fighting over a truck,
not a deliberate takeover. Anything beyond a simulator needs per-driver tokens
scoped to the driver's own path.

## The simulator

The trucks listed in `sim/trucks.json` share one CARLA map, so they can see
each other. The worker is Python because CARLA's own client is a Python wheel
and moq's Python bindings (built from `py/` in this checkout by
`just carla venv`) cover publishing and the JSON tracks. CARLA advises a single
command-sending client, and one process keeps Traffic Manager coherent.

- **Video follows demand.** A map preview subscribes to `sd` and the focused
  view to `hd`. The simulator renders only the camera covering the largest
  demanded rendition, scales the rest from it on the GPU, and parks the camera,
  its encoders, and the world renderer once the last subscriber leaves.
- **So does the clock.** Stepping physics costs most of a core with rendering
  parked, so the world stops once no viewer, status subscriber, or driver
  needs it.
- **The world runs at a fixed step.** CARLA is in synchronous mode at 1/30 s,
  so every frame is stamped exactly a frame apart instead of with however long
  the last tick took. The worker returns the world to free-running on exit.
- **An idle simulator parks.** The server boots with rendering on and outlives
  every client; `just carla park` disables rendering unless a worker holds the
  simulator.

Bake a town's roads for the player's map against a running simulator with
`just carla map`, and commit what it writes to `web/src/maps/`. See
[`web/src/maps/README.md`](web/src/maps/README.md) for an aligned aerial image.

## The GPU video bridge

Camera pixels never leave the GPU. `bridge/` is a library the simulator loads:
the CARLA plugin ([moq-dev/carla](https://github.com/moq-dev/carla)'s
`plugin/MoqBridge`) copies each camera's render target
into an exportable Vulkan image on Unreal's own queue, and the bridge hands it
to the `moq-video` encoder on the same device, which converts, scales, and
encodes it there. Compressed frames cross a Unix socket to the worker
(`sim/moq_carla_sim/bridge.py`), which publishes them on `sd` and `hd`. There
is no CPU path: a camera that cannot reach the bridge fails its spawn, and the
worker exits for systemd to restart.

- **Bounded.** Each camera cycles a few slots; with none free it drops the
  picture before anything encodes it, and a worker that falls behind loses
  packets only up to the next keyframe.
- **Tested without CARLA.** A Vulkan stand-in for the plugin drives the
  bridge: `just carla bridge-gpu` runs it on an NVIDIA host, and
  `cargo run -p moq-carla-bridge --example standin -- <socket> truck1/front`
  feeds a worker by hand. The plugin itself compiles only in the CARLA build.

The C ABI the plugin calls is `bridge/include/carla_bridge.h`. moq-dev/carla
holds an identical copy that the plugin compiles against: change both
together, since its image build refuses a mismatch.

## Keeping it running

On a Linux workstation, the simulator and worker run as rootless systemd user
units. The installer records this checkout's path; rerun it after moving the
checkout.

```bash
just carla venv
just carla service-install

# The units read this. MOQ_URL carries a token allowed to publish `carla/**`
# and subscribe `viewer/carla/**`; keeping it here keeps it off the command
# line. CARLA_IMAGE is the tag the moq-dev/carla build printed, since the worker
# refuses a stock simulator.
cat >~/.config/moq/carla.env <<'EOF'
MOQ_URL=https://cdn.moq.dev/demo?jwt=...
CARLA_IMAGE=localhost/moq-carla:0.9.16-bridge-...
EOF
chmod 600 ~/.config/moq/carla.env

just carla service-enable
just carla service-status
just carla service-logs
```

`carla-sim.service` owns the podman container and `carla-worker.service` the
Python process; both restart with capped backoff. Run
`sudo loginctl enable-linger "$USER"` once to start them at boot rather than
at login.

## The player

The player is a `<moq-carla>` custom element: importing
`@moq/demo-carla/element` registers it, and its `url` attribute names the relay.
`just carla web` serves it on a local dev page. With no simulator publishing,
it says so.

Hover a truck on the map to stream its 240p preview; click it for 720p. A drive
key, a wheel movement, or **Take control** claims the lease, and **Esc**
releases it. Inputs ramp rather than snap and hold where you leave them:
**A**/**D** steer, **C** recenters, **W** releases the brake then feeds
throttle, **S** cuts throttle then brakes, **Space** or **G** changes gear,
**E** e-stops, and **R** resets the e-stop. **Reset** returns the focused truck
to a free spawn point under autonomy.

A steering wheel works through the
[Gamepad API](https://developer.mozilla.org/docs/Web/API/Gamepad_API). Unlike
the keys it is absolute: rim angle is steering angle and pedal travel is
throttle and brake. It takes over only once it moves with the throttle
released, so a resting foot cannot claim control, and losing an armed wheel
cuts the throttle and brakes. Click its name in the header to calibrate an
unfamiliar layout; the result is stored per device.

## Checks

`just carla check` lints and tests the worker and its launchers without CARLA
or a GPU. `just js` and `just rs` cover the player and the bridge, and
`just carla bridge-gpu` runs the bridge's hardware test.
