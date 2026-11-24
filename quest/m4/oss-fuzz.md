# [M] moq's fuzz targets run continuously on OSS-Fuzz

## Goal

OSS-Fuzz builds moq's fuzz targets from `main` and fuzzes them around the
clock, keeping its own corpus. A crash it finds reaches the maintainer
privately, and its reproducer lands in the target's `regressions/` so
`just check` replays it.

## Plan

Decided 2026-11-10 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq#8472](https://github.com/moq-dev/moq/pull/8472)):

- ✅ OSS-Fuzz (rejected: longer runs on our own runners, which cost runner
  time and stay far short of continuous; dropping it).
- ✅ m4, after the whole-session targets exist, since stateful targets gain
  the most from long runs.

Decided 2026-11-11 (maintainer, /quest-unblock): ✅ the project files list
`security@moq.dev`, the address in `SECURITY.md` (rejected: a personal
address), and ✅ the agent opens the google/oss-fuzz PR from the
maintainer's account once the maintainer has reviewed its draft files and PR
text (rejected: waiting to decide). The maintainer accepts OSS-Fuzz's 90-day
disclosure deadline.

Nightly gives each of its 24 codec targets five minutes and the two
whole-session targets (`session_lite`, `session_ietf`) fifteen. That finds
shallow bugs and replays the corpus; it rarely reaches deep states, and the
session targets' coverage still climbs when their run ends.

- The OSS-Fuzz project files (`project.yaml`, `Dockerfile`, `build.sh`) live
  in google/oss-fuzz. The build script calls `cargo fuzz build` per crate
  with the same profile overrides `sh/rs/fuzz.sh` sets, so keep those in one
  place both can use.
- Seed each target from `fuzz/regressions/` and the generated seeds.
- Enable every target in Nightly's matrix; say which, if any, OSS-Fuzz's
  toolchain cannot build.
- Add ClusterFuzzLite or OSS-Fuzz's CIFuzz action to PRs only if it stays
  cheap; Nightly's five-minute run already covers regressions.
- Say in `CONTRIBUTING.md` or the fuzz README how a reported crash is
  handled: reproduce, fix on `main`, commit the input, backport when serious.

Public API: none. Wire: none.
