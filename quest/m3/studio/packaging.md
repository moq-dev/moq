# [M] Studio packaging

## Goal

Every release ships a signed and notarized macOS app, a signed Windows
installer, and a Linux Flatpak of `moq-studio`, all built in CI. Each artifact
is verified to launch and list devices on its platform.

## Plan

- The Flatpak builds with `--features capture,pipewire`. Inside the sandbox
  the camera portal is the only way to a camera, and its PipeWire DMA-BUFs
  are what the compositor imports without a copy. `moq-scene` and
  `moq-studio` keep `pipewire` off by default because it links libpipewire
  at build time.

## Required

- [Signing identities are available](/quest/m3/studio/signing.md) - the credentials signing needs
- [Studio on macOS and Windows](/quest/m3/studio/app.md) - the studio running on every platform it packages

## Related

- [Ship capture and playback](/quest/m2/cli-packaging.md) - the same distribution checks for the `moq` binary
