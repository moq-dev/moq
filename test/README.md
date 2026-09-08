# Cross-language tests

Harnesses that span more than one language or need a server. Per-language unit
tests live in each language's own justfile.

| Harness | Entry point | What it proves |
| --- | --- | --- |
| [smoke](smoke/README.md) | `just test smoke` | every client built from this checkout interoperates |
| [wasm](wasm/README.md) | `just test wasm` | the `@moq/wasm` bindings work in a real browser |
| [ts](ts/README.md) | `just test ts` | the subscriber's `export ts` output is IRD-compliant |

All three stand up a `moq-relay` and clients, so two of them running at once, or
the same one running from two worktrees, would otherwise collide. `lib/harness.sh`
is what keeps them apart.

## What a run owns

Every run owns three things and touches nothing else.

**A private run directory.** `mktemp -d` under `$TMPDIR/moq-test-<uid>`, mode 700,
holding every log, generated config, and capture. `MOQ_TEST_RUNS` moves the root.

**Reserved ports.** A port is claimed by creating a directory under
`$TMPDIR/moq-test-ports-<uid>` (`MOQ_TEST_PORTS`), held for the whole run, and
released on the way out. That reservation is the point: probing for a free port and then
releasing it is a race, and two runs that probe at the same moment pick the same
number. The walk starts at `MOQ_TEST_PORT_BASE` (4500). A reservation whose owner
process is gone is reclaimed, atomically, so two reclaimers cannot both win.
Replacement is serialized by `flock` on Linux or `lockf` on macOS, and the
reservation records the owner's process start so a reused PID is not mistaken
for the original run.

Both roots carry the user id because `TMPDIR` is usually unset on Linux: a fixed
name in a world-writable `/tmp` belongs to whoever ran first, and everyone else
would fail to create anything under it. Two worktrees still share, since they run
as the same user, which is what makes the reservations mean anything.

The reservation settles contention between harness runs, not with the rest of the
machine, so each harness still refuses a port something unrelated is already
serving on. Pinning a port (`SMOKE_PORT`, `WASM_PORT`, `TSC_PORT`, `--port`) takes
that exact one or fails.

Every port, pinned or walked to from `MOQ_TEST_PORT_BASE`, has to be 1024..65535
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

```bash
MOQ_TEST_KEEP=1 just test smoke
```

The run directory survives with its logs, configs, and `endpoints.txt`, and the
path is printed along with the command that reproduces the run. The children are
still reaped and the ports still released: what is kept is evidence, not a live
session. Remove it with the `rm -rf` the run prints; nothing expires it for you.

## Worktrees

`just worktree` reports what a checkout can actually do before anything is built:
its base and how stale that base is, and whether the Git metadata is reachable for
fetch, branch creation, and rebase. A linked worktree keeps its shared metadata
under `--git-common-dir`, inside the main repository, so write access to the
source tree does not imply any of the three.

```bash
just worktree          # report, change nothing
just worktree setup    # fetch, set the branch upstream, record the base SHA
```

`setup` never resets, rebases, or cleans, so it is safe to run against a checkout
with work in progress.

The upstream is the only place the base survives, so `setup BASE` fails when it
cannot be written -- a detached HEAD has no branch to hang it on, and the shared
config may be read-only. Left as a warning, `just check` would go on scoping
against `origin/main` while setup reported success. Without a `BASE`, that
fallback is what would have been written anyway, so it warns instead.
