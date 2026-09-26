# [M] Go mirror delivery

## Goal

A recommendation, with a prototype, for delivering the Go binding's
staticlibs without committing them to git. `moq-dev/moq-go-ffi` commits
them straight into git today, at 60.2 MiB for linux, 52.7 for windows, and
39.1 for darwin. That puts the largest file at 60% of GitHub's 100 MB push
limit, and each release adds about 210 MiB of history.

## Plan

- Evaluate what keeps `go get` working without a separate download step, and
  what doesn't:
  - one module per platform
  - history that squashes or orphans old releases
  - a module proxy we host
  - release assets fetched by a build step
  Git LFS is out, because the Go module proxy doesn't serve LFS objects.
- The release profile quest shrinks the staticlibs (121 to 41 MiB on macOS),
  which buys headroom but doesn't stop the history growth.
- Deliverable: the chosen approach recorded here, and a follow-up quest to
  implement it. Split this if the prototype turns out large.

## Related

- [Release profile](/quest/m1/release-profile.md) - shrinks what the mirror carries
