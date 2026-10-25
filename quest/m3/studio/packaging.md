# [M] Studio packaging

## Goal

Every release ships a signed and notarized macOS app, a signed Windows
installer, and a Linux Flatpak of `moq-studio`, all built in CI. Each artifact
is verified to launch and list devices on its platform.

## Required

- [Signing identities are available](/quest/m3/studio/signing.md) - the credentials signing needs
- [Studio program encode from the GPU](/quest/m3/studio/app.md) - the studio running on every platform it packages

## Related

- [Ship capture and playback](/quest/m2/cli-packaging.md) - the same distribution checks for the `moq` binary
