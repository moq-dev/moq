# Cross-language tests

Harnesses that span more than one language or need a server. Per-language unit
tests live in each language's own justfile and are selected by `just check`.
Run `just --list test` for the full recipe list; the table below routes common
questions to a harness. For builds and CI selection, see the
[development guide](../doc/setup/dev.md#checks-and-tests).

| Harness | Entry point | What it proves |
| --- | --- | --- |
| [interop](interop/README.md) | `just test interop --all` | clients built from this checkout interoperate; without `--all`, only Rust runs |
| [wire compatibility](interop/README.md) | `just test wire-compat` | this checkout exchanges data with published versions |
| [IETF conformance](ietf-conformance/README.md) | `just test ietf-conformance` | pinned Moxygen sections 1-7, under the relay conformance profile, preserve subgroup-zero objects through one relay |
| [registry smoke](smoke/README.md) | `just test smoke` | published packages install and interoperate; see the guide for matrix selection |
| [browser media](interop/README.md) | `just test media` | browser capture and playback deliver video and audio through publication lifecycle changes |
| [failure drills](drill/README.md) | `just test drill` | relay and transport failures terminate or recover as expected |
| [ts](ts/README.md) | `just test ts` | the subscriber's `export ts` output is IRD-compliant |
| [native-play](native-play/README.md) | `just test native-play` | the shipped headless player follows overlapping publishers and fallback through decoded PCM on a private Linux null audio device |
| [audio-quality](audio-quality/README.md) | `just test audio-quality` | browser and native audio playout stay within budget over an impaired path |
| [drain](drain/README.md) | `just test drain` | a JS viewer migrates off a draining relay without a dropped group |
| qmux | `just test qmux` | `@moq/qmux`, as client and as server, interoperates with the Rust qmux 0.6.3 release on every QMux draft |
| video | `just test video` | the `@moq/video` renderer paints through WebGPU and falls back to Canvas2D after losing the GPU |
| [json-properties](json-properties.md) | `just test json-properties` | both Snapshot codecs reconstruct, and both Window codecs emit the exact events for, random JSON sequences with replayable seeds |

Harnesses that stand up a relay and clients need isolated ports and processes
when run concurrently or from different worktrees. The shared
[`lib/harness.sh`](lib/harness.sh) provides that isolation.

## What a run owns

Every run owns three things and touches nothing else.

**A private run directory.** `mktemp -d` under `$TMPDIR/moq-test-<uid>`, mode 700,
holding every log, generated config, and capture. `HARNESS_RUNS` moves the root.
Client builds go here too (the Python venv, the staged Go modules, the browser
page), so two runs from one checkout never rebuild a client the other is running.

**Reserved ports.** A port is claimed by creating a directory under
`/tmp/moq-test-ports-<uid>` (`HARNESS_PORT_ROOT`), held for the whole run, and
released on the way out. That reservation is the point: probing for a free port and then
releasing it is a race, and two runs that probe at the same moment pick the same
number. The walk starts at `HARNESS_PORT_BASE` (4500). A reservation whose owner
process is gone is reclaimed, atomically, so two reclaimers cannot both win.
Replacement is serialized by `flock` on Linux or `lockf` on macOS, and the
reservation records the owner's process start so a reused PID is not mistaken
for the original run.

The reservation root ignores `TMPDIR`: shells may set different temporary
directories but share the host's ports. The user id avoids ownership conflicts in
world-writable `/tmp`. An override via `HARNESS_PORT_ROOT` must be the same for every
run sharing the network. The root must belong to the current user and cannot be
a symlink; new roots are private. Runs by different users still need disjoint
ports.

The reservation settles contention between harness runs, not with the rest of the
machine, so each harness still refuses a port something unrelated is already
serving on. Pinning a port (`INTEROP_PORT`, `TSC_PORT`, `--port`) takes
that exact one or fails.

Every port, pinned or walked to from `HARNESS_PORT_BASE`, has to be 1024..65535
before a reservation is created. A reservation is a directory named after the
port, so the check keeps a malformed value from naming somewhere else on the
filesystem, and it refuses an unbindable number up front instead of after the
build, as a relay that never became ready.

**Process groups.** Every child is spawned as its own process group leader, and
teardown signals the group. A group catches grandchildren -- ffmpeg behind a
pipe, Chromium behind Playwright, `tsp` behind `timeout` -- including ones that
reparent, which a `pgrep -P` walk down the tree misses. A group is signalled only
until it has been waited on, so a PID the OS later recycles is never touched.

Nothing in a run is discovered by scanning the machine, so a run cannot reap
another run's relay, and cancelling one leaves the other working.

## Teardown

Normal exit, `^C`, and `SIGTERM` all run the same teardown: reap the process
groups, release the reservations, remove the run directory. Startup failure takes
the same path, so a run cancelled before its relay ever bound leaves nothing
behind either.

Teardown never touches the source tree, another worktree, or a shared cache. Nor
does `just clean`, which cleans this checkout only; `just clean all` opts into
agent worktrees and is for a machine you know is idle, since `cargo clean` under
a running build fails it.

### Keeping a failure

A run that fails keeps its directory, so its logs, configs, `endpoints.txt`, and
any Playwright trace survive with the command that reproduces it. A passing run
deletes its own.

```bash
HARNESS_KEEP=1 just test interop
```

`HARNESS_KEEP=1` keeps a passing run's directory too, for when the problem is in
what the test did not assert. Either way the children are still reaped and the
ports still released: what is kept is evidence, not a live session. Remove it
with the `rm -rf` the run prints; nothing expires it for you.

In CI the harness writes under `HARNESS_RUNS`, and `interop.yml`
uploads that directory as a short-lived artifact when the job fails.

`HARNESS_RESULTS` names a JSON Lines file a harness appends one record per case to,
through `harness_result`; `interop.yml` builds its report from it. See
[interop/README.md](interop/README.md#ci).
