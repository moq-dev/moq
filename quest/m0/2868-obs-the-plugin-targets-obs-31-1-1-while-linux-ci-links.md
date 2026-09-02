# [S] obs: the plugin targets OBS 31.1.1 while Linux CI links against 32

## Goal

The libobs the released macOS and Windows plugin binaries link against is the
same major as the one the Linux compile gate checks, so a libobs API change
cannot pass CI and surface only in a release build on the platforms with no
gate.

## Plan

`cpp/obs/buildspec.json` pins obs-studio 31.1.1, which is what obs-deps hands
a macOS or Windows build. `flake.nix`'s `libobs-headers` pins the same, and
`just obs check` fails if the two drift. Neither is what Linux links: the
`obs.yml` gate uses nixpkgs' obs-studio, a major release ahead, and nothing
compares that third version to the other two.

- Bump `buildspec.json` and `libobs-headers` together to the current stable,
  32.2.2 as of 2026-08-14: new hashes for the obs-studio, prebuilt obs-deps,
  and Qt6 archives (macOS and Windows) plus the nix `fetchzip` hash.
- Check `libobs/obsconfig.h.in` and `frontend/api/obs-frontend-api.h` are
  still where `libobs-headers` expects them.
- Extend the drift guard to the nixpkgs version `just obs ci` links against,
  so the next gap is a failing check rather than a discovery.
- A real `just obs build` on macOS, since that is the platform the release
  ships and PR CI never compiles.

## Closes

- [#2868](https://github.com/moq-dev/moq/issues/2868) - close this issue when the quest finishes
