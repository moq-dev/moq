# [S] Long test runs keep their built binaries

## Goal

A long `just test interop` or wire-compat run never fails because the
binaries it built were removed from the shared Cargo target mid-run, such
as by mbx pruning its managed build storage.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7211](https://github.com/moq-dev/moq/pull/7211). Milestone: ✅ m0 (user, "all recommended" for quests 1-8).

- **Target removed mid-run**: the 2026-10-31 run reported the shared
  `target/` deleted by mbx under long interop and wire-compat runs. mbx
  links each checkout's target to managed storage that prunes itself
  (`mbx gc` collects managed targets to fit its budget), and
  `test/interop/interop.sh:578` (at be39188000) resolves `TARGET_BASE`
  once, then runs `moq-relay`, `moq-cli`, its examples, and moq-c's library
  from there for the whole matrix; `test/interop/compat.sh:72-73` does the
  same. Confirm from mbx's logs or `mbx gc --dry-run` which collection
  removes them. Then either copy everything the run loads from the target
  into the run's own directory (`$HARNESS_RUN`) right after building,
  which needs no mbx change: binaries, examples, moq-c's headers and
  library, and any GStreamer plugin the matrix loads, with every consumer
  pointed at the copies. Or use an mbx lease or pin if mbx offers one.
  Prefer whichever keeps one rule for every long-running script.

The one-off `Argument list too long` Cargo launch #7211 saw is fixed:
[moq-dev/moq#7594](https://github.com/moq-dev/moq/pull/7594) (merged)
traced it to mbx copying a PATH that grew with every nested dev shell and
dedupes PATH in `flake.nix`'s `shellHook`.

Public API: none. Wire: none.

## Related

- [mbx's free-space GC never removes a live target](/quest/m0/ci/mbx-gc.md) - the host-side fix for the same GC, which also removed a live target outside interop
