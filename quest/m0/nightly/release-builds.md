# [S] Nightly's moq-gst builds survive an apt mirror outage

## Goal

`moq-gst`'s Nightly builds stop failing when Ubuntu's apt mirrors can't be
reached, so `Shipping sizes` still runs on those nights.

## Plan

On 11-11 an apt DNS failure (`Temporary failure resolving
'security.ubuntu.com'`) failed both `moq-gst / Build tarball` jobs and both
`Package .deb/.rpm` jobs at their apt step, and so skipped `Shipping sizes`.
That apt use is deliberate. The tarball job smoke-tests the plugin against
the distro's own GStreamer, and the package job links against the distro's
GStreamer headers. Nix can't stand in for either.

Decided 2026-11-13 (maintainer, /quest-complete): ✅ the Linux jobs take the
distro GStreamer from a weekly-published ghcr.io image with the packages
preinstalled (rejected: accepting a mirror outage as a real failure).
`gstreamer-ci.yml` publishes it as `ghcr.io/moq-dev/gstreamer-ci` and has
moq-bot propose the digest pin, `.github/docker/gstreamer/image`, from the
`gstreamer-ci/pin` branch.

- Tarball job: on Linux, run `just rs gst-smoke` over the extracted tarball
  inside the pinned image with `docker run`, instead of apt-installing
  GStreamer on the runner. macOS keeps `brew install gstreamer`.
- Package job: run only the `cargo zigbuild` step inside the image, with the
  checkout, `$HOME` (rustup's toolchain), and `/nix` (the packaging tools)
  mounted, so pkg-config finds the image's GStreamer. Don't make it a
  `container:` job: Nix's installer and the composite actions would then run
  inside the image too.
- Read the pin from the file, log in to ghcr.io with `GITHUB_TOKEN`, and
  grant `packages: read` in `moq-gst.yml` and in Nightly's `release-moq-gst`
  caller.
- Drop the apt steps. Validate with a `workflow_dispatch` of `moq-gst.yml`
  on the quest branch, which publishes nothing off a tag.

The other release-build failures from 11-11 to 11-12 are fixed: armv7 by
[#8509](https://github.com/moq-dev/moq/pull/8509), the Go bindings by
[#8585](https://github.com/moq-dev/moq/pull/8585), and the FFI aarch64
cross toolchain by
[moq-dev/moq#8668](https://github.com/moq-dev/moq/pull/8668).

## Waiting

- moq-dev/moq: the pin file `.github/docker/gstreamer/image` is on `main`, from moq-bot's `chore(ci): pin the newest GStreamer CI image` PR, which the first `gstreamer-ci.yml` run on `main` opens; check that run published both amd64 and arm64
