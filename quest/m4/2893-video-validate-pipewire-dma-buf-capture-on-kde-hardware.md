# [M] video: validate PipeWire DMA-BUF capture on KDE hardware

## Goal

Implement and verify the behavior tracked in [#2893](https://github.com/moq-dev/moq/issues/2893)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Problem

Hardware validation for #2839 did not complete on a KDE/Wayland desktop. The screen source was selected in the portal, but the ignored PipeWire capture test never received a frame and timed out after 120 seconds.

This is a focused follow-up to the broader Linux zero-copy tracker in #2819.

#### Environment

- KDE desktop on Wayland
- `XDG_CURRENT_DESKTOP=KDE`
- `WAYLAND_DISPLAY=wayland-0`
- `DISPLAY=:0`
- `XDG_RUNTIME_DIR=/run/user/1000`
- Active desktop D-Bus session

#### Reproduction

Run from the Nix development shell after #2839:

```sh
cargo nextest run --profile ci -p moq-video --all-features --run-ignored ignored-only portal_captures_frames --no-capture
```

During review, a temporary assertion also required captured frames to be `Surface::DmaBuf`. The run reached the test, the portal source was selected, then nextest reported the test as slow after 60 seconds and terminated it at 120 seconds without receiving a frame.

#### Expected

- Portal selection completes.
- PipeWire format and buffer negotiation completes.
- The first frame arrives within the existing capture timeout.
- A DMA-BUF-capable compositor produces `Surface::DmaBuf`; shared memory remains a working fallback.

#### Validation

- \[ ] Reproduce the post-selection timeout on KDE/Wayland.
- \[ ] Add tracing around portal completion, PipeWire connection, format fixation, buffer allocation, and first-frame delivery to locate the stall.
- \[ ] Validate packed RGB DMA-BUF capture and Vulkan import on Intel or AMD hardware.
- \[ ] Validate the linear DMA-BUF CPU fallback.
- \[ ] Confirm shared-memory PipeWire capture still works when DMA-BUF is unavailable.
- \[ ] Hold multiple frames long enough to exercise buffer leasing without pool exhaustion or reused content.
- \[ ] Add or refine an ignored hardware test so the observed failure is distinguishable from a portal-selection timeout.

Refs #2839 and #2819.

## Closes

- [#2893](https://github.com/moq-dev/moq/issues/2893) - close this issue when the quest finishes

## Related

- [#2819: moq-video: carry PipeWire DMA-BUFs safely into the Vulkan renderer](/quest/m3/2819-moq-video-carry-pipewire-dma-bufs-safely-into-the-vulkan.md) - related open work
