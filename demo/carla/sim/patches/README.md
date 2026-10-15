# CARLA source builds

The worker needs a CARLA simulator built from source with the patches here,
applied to clean CARLA commit `294096eb1c38eabf246e4f3a9cdab704e33a7f4c`
(release 0.9.16) and CARLA's UE4 engine fork at
`e9d9e60c85f643e10eeb03f42f61554d18dcb30f`.

## The patches

`rpc-idle-wait.patch` fixes an idle CPU spin. `FCarlaEngine::OnPreTick` loops
over `Server.RunSome(1u)` until the next synchronous tick cue, and
`carla::rpc::Server::SyncRunFor` resets its Asio context and calls
`run_for(1ms)` with no outstanding work guard. An empty context returns
immediately, so the game thread busy-spins between the worker's ticks. The patch
adds a slice-local [work guard](https://www.boost.org/doc/libs/1_84_0/doc/html/boost_asio/reference/io_context.html),
so each slice waits out its existing time budget for RPC arrivals, plus a
regression in CARLA's RPC suite that fails without it.

Measured with `-nullrhi`, process and game-thread CPU from `/proc` jiffies after
a 30 s warmup:

| State | Stock process | Stock game thread | Patched process | Patched game thread |
| --- | --- | --- | --- | --- |
| No client, 30 s | 133.1% | 90.3% | 33.6% | 21.8% |
| Rendering parked, 30 s | 133.2% | 89.9% | 28.1% | 17.7% |
| Synchronous 30 ticks/s, 10 s | 101.3% | | 4.4% | |

Both advanced all 300 synchronous frames; mean tick RPC went from 0.47 to
1.14 ms, maximum 0.72 to 1.90 ms. Unreal's own frame caps (`-fps=30`,
`t.MaxFPS 30`) do not bound the idle server: CARLA's loop overrides both.

The GPU video bridge is three pieces on the same revisions:

- `bridge-camera.patch` (CARLA): the `moq_bridge_stream` attribute on
  `sensor.camera.rgb`. Such a camera registers with the bridge at BeginPlay
  (refusing its spawn when it cannot), ticks while the bridge wants it rather
  than while a client listens, and hands its render target to the bridge
  instead of `FPixelReader`. It also lists the MoqBridge module in
  `Carla.uplugin`, loaded at `PostConfigInit`.
- `../plugin/MoqBridge` (copied into the Carla plugin with the bridge's C
  header): enables `VK_KHR_external_memory_fd`, `VK_KHR_external_semaphore_fd`
  and `VK_KHR_timeline_semaphore` on Unreal's device when the simulator runs
  with `-moq-bridge-socket=`, allocates exportable slots on it, and records
  each copy into Unreal's active command buffer, restoring the render target's
  tracked layout, followed by an empty submission that signals the slot's
  timeline. It loads `libcarla_bridge.so` from beside the executable.
- `engine-timeline-semaphore.patch` (engine): chains the timeline-semaphore
  feature into `FVulkanDevice::CreateDevice` when its extension is enabled,
  which no plugin hook reaches.

## Building

`just carla build` runs [`build.sh`](build.sh): clean CARLA plus
`rpc-idle-wait.patch`, `bridge-camera.patch` and the MoqBridge module, on the
engine plus `engine-timeline-semaphore.patch`, compiled by
[`compile.sh`](compile.sh) in the [builder image](builder.Containerfile) as
rootless UID 1000. The bridge library (`demo/carla/bridge`, a workspace member)
is built first, in a digest-pinned Debian bullseye Rust image, because the
published image is Ubuntu 20.04 with glibc 2.31 and bullseye has the same. The
[runtime image](runtime.Containerfile) overlays the executable, the library and
the four configuration files onto the published image, and the script prints
its tag, `localhost/moq-carla:0.9.16-bridge-<executable>-<library>` with the
first 12 hex of each SHA-256, both also image labels. It needs `gh` access to
the Epic-gated engine (GitHub serves it only to accounts linked to an Epic
account), about 40 GB, and takes roughly 20 minutes cold on a 32-core machine,
most of it the Shipping build. Private engine sources and generated binaries
are not distributed here.

Everything it fetches and compiles stays in `~/.cache/moq-carla-build`
(`CARLA_BUILD` overrides it), with a log per run under `logs/`, so a re-run
resumes rather than starting over. A fetch lands in a staging directory renamed
into place only on success, so a failed one (no Epic authorization, a dropped
connection) never blocks the next run. A cached checkout may hold only its
patches, the module and the build's own scripted edits: [`sources.sh`](sources.sh)
refuses anything else rather than compile it into an image labelled as this
build. The build may share its host with a running simulator, so it runs at idle
CPU and I/O priority under a memory cap; `NINJA_JOBS`, `UE_JOBS` and
`BUILD_MEMORY` tune it.

Unreal compiles the copy of `rpc/Server.h` that `BuildLibCarla.sh --server`
installs into `Plugins/Carla/CarlaDependencies`, not LibCarla's source, so
editing the source alone rebuilds the old behavior. `compile.sh` applies the
patch before that install and fails unless the installed header matches, then
runs the RPC suite, including the patch's `idle_sync_slice_waits_for_work`.
CARLA reports the version of a `ue4/<release>` branch and otherwise its short
hash, which the worker refuses as a mismatch, so the checkout sits on
`ue4/0.9.16`.

Pin the result with `CARLA_IMAGE` in `~/.config/moq/carla.env`, which
`carla-sim.service` reads and `just carla service-install` does not
overwrite; `server.sh` logs the image it resolved, and the worker refuses a
simulator without the bridge by name.

### What the script does

These are the steps `build.sh` and `compile.sh` automate, for anyone adapting
them. The builder is Ubuntu 22.04 with the packages in
[`builder.Containerfile`](builder.Containerfile); the engine installs its own
v17 clang 10.0.1 CentOS 7 SDK, and host clang is not used.

1. Fetch engine dependencies with `Engine/Build/BatchFiles/Linux/GitDependencies.sh --force --exclude=Win32 --exclude=Win64 --exclude=Mac --exclude=Android --exclude=IOS --exclude=TVOS --exclude=HoloLens`. The outer setup script
   combines `--force` with `--prompt` and silently skips dependency fetching.
2. CARLA's libpng 1.6.37 download moved under SourceForge's
   `libpng/libpng16/older-releases/1.6.37/` directory, so that URL is updated in
   `Util/BuildTools/Setup.sh`. Setup runs for Python 3.10, then
   `BuildLibCarla.sh --server --release` and `BuildOSM2ODR.sh --build`.
3. Chrono, ROS2, PyTorch, CarSim and SimReady are disabled in
   OptionalModules.ini, with Unity ON, as are the MDL/SimReady plugins in
   CarlaUE4.uproject and the SimReady dependency in CarlaTools.uplugin.
4. The CarlaUE4 target is built directly from the engine checkout:

   ```sh
   bash Engine/Build/BatchFiles/Linux/Build.sh CarlaUE4 Linux Shipping \
     -Project=/work/carla/Unreal/CarlaUE4/CarlaUE4.uproject -MaxParallelActions=8
   ```

The runtime uses the published image's cooked assets, with the executable and
the configuration files above replaced; no map recook is performed. Source
executables report 0.9.16 for client compatibility, so identify a build by its
hashes, never that version string alone.
