# [S] obs: the plugin targets OBS 31.1.1 while Linux CI links against nixpkgs' 32.1.2

## Goal

Implement and verify the behavior tracked in [#2868](https://github.com/moq-dev/moq/issues/2868)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Split out of a review finding on #2867 (CodeRabbit flagged the stale pin; the divergence below is what makes it worth its own issue).

#### The two versions

- `cpp/obs/buildspec.json` pins **obs-studio 31.1.1**. That's what the obs-deps download gives a macOS or Windows build, so it's the libobs the *released* plugin binaries link against.
- nixpkgs currently carries **obs-studio 32.1.2**, which is what a Linux `just obs build` and the new `obs.yml` gate link against.
- `flake.nix`'s `libobs-headers` also pins 31.1.1, matching buildspec.json (#2867 added a guard in `just obs check` that fails if those two drift).

So the Linux compile gate and the shipped macOS/Windows binaries are a major OBS release apart. Nothing has broken yet, but that gap is exactly where a libobs API change gets through CI and shows up only in a release build, on the platforms with no compile gate at all.

#### What to do

Bump `cpp/obs/buildspec.json` to the current stable (32.2.2 at time of writing) and move `libobs-headers` in `flake.nix` in the same commit. Needs:

- new hashes for the obs-studio, prebuilt obs-deps and Qt6 archives (macOS + Windows entries), and the nix `fetchzip` hash;
- a check that `libobs/obsconfig.h.in` and `frontend/api/obs-frontend-api.h` are still where `libobs-headers` expects them;
- a real `just obs build` on macOS, since that's the platform the release actually ships and the one PR CI never compiles.

`just obs check` will fail until both pins move together, which is the intended tripwire rather than an obstacle.

## Closes

- [#2868](https://github.com/moq-dev/moq/issues/2868) - close this issue when the quest finishes
