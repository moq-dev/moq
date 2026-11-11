# [M] moq's fuzz targets run continuously on OSS-Fuzz

## Goal

OSS-Fuzz builds moq's fuzz targets from `main` and fuzzes them around the
clock, keeping its own corpus. A crash it finds reaches the maintainer
privately, and its reproducer lands in the target's `regressions/` so
`just check` replays it.

## Waiting

- Maintainer: approve the application PR to
  [google/oss-fuzz](https://github.com/google/oss-fuzz) and name the contact
  address it lists. An agent cannot take this: it posts outside the
  `kixelated` and `moq-dev` orgs, the address receives private security
  reports, and accepting commits the project to OSS-Fuzz's 90-day disclosure
  deadline. Done when the application PR is open.

## Plan

Decided 2026-11-10 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq#8472](https://github.com/moq-dev/moq/pull/8472)):

- ✅ OSS-Fuzz (rejected: longer runs on our own runners, which cost runner
  time and stay far short of continuous; dropping it).
- ✅ m4, after the whole-session targets exist, since stateful targets gain
  the most from long runs.

Nightly gives each of its 22 targets five minutes. That finds shallow bugs
and replays the corpus; it rarely reaches deep states.

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

## Required

- [A hostile peer's session cannot panic, leak, or disturb another broadcast](/quest/m1/session-fuzz.md) - the stateful targets that gain most from continuous fuzzing

## Related

- [Wire fuzz coverage](/quest/m4/wire-fuzz-coverage.md) - adds the moq-sock and moq-uring targets OSS-Fuzz would also run
