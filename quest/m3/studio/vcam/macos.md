# [M] macOS virtual camera

## Goal

The macOS app bundle carries a CoreMediaIO Camera Extension. Once the user
approves it, apps list a camera that carries a canvas's program. An app
update installs the new extension without a reboot.

## Required

- [Signing identities are available](/quest/m3/studio/signing.md) - system extensions only load when signed
- [Studio program encode from the GPU](/quest/m3/studio/app.md) - the studio running on macOS
