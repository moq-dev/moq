# [XS] Nightly's moq-gst builds survive an apt mirror outage

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

The other release-build failures from 11-11 to 11-12 are fixed: armv7 by
[#8509](https://github.com/moq-dev/moq/pull/8509), the Go bindings by
[#8585](https://github.com/moq-dev/moq/pull/8585), and the FFI aarch64
cross toolchain by the PR that rewrote this quest.

## Waiting

- Maintainer: decide how moq-gst gets the distro GStreamer.
  (a) Recommended: run the two Linux jobs in a container image with the apt
  packages preinstalled, published to ghcr.io by a weekly job. That trades
  the mirror fetch for a registry pull that GitHub already depends on.
  (b) Accept a mirror outage as a real failure and leave it as it is.
  Agents can't pick between these, because one adds an image to maintain
  and the other accepts occasional red nights.
