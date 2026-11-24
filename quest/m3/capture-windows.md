# [M] Windows capture parity

## Goal

System audio works on Windows. App capture stays macOS-only (decided below),
which `Source::App` already reports as `Unsupported` here. Display and window capture,
including the cursor, already run on the Windows.Graphics.Capture backend
(`rs/moq-video/src/capture/wgc/native.rs`), which
[its own quest](/quest/m3/capture-wgc.md) verifies on real hardware.

## Plan

- **Applications** (every window of a process, including ones that open later)
  are still `Unsupported` outside macOS, where `SCShareableContent` gives it
  almost free. Decided 2026-11-15 (maintainer, /quest-complete, with
  [Linux capture parity](/quest/m3/capture-linux.md)): ✅ (A) Windows offers
  window capture only: `Source::App` stays macOS-only and fails with its
  reason here, and a listing uses apps only to group windows
  (rejected: (B) build app capture by composing a process tree's windows). No
  consumer asks for it, and it matches the
  [capture listing](/quest/m3/capture-list.md)'s rule that a listed app must
  not hand out a source that fails at open.
- **System audio** needs WASAPI loopback. `moq_audio::capture::Source::System`
  exists but is macOS-only and returns `Unsupported` elsewhere. Unlike macOS
  this does not go through the screen-capture API, so it is an independent path
  and does not inherit the Screen Recording permission coupling.
- **Per-app audio** (decided 2026-10-26, maintainer: yes) uses WASAPI process
  loopback (`AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`) to capture one
  process tree's output, through the same `moq_audio::capture` source as the
  Linux and macOS halves. Microsoft documents build 20348 as the floor, though
  it works from Windows 10 2004 (build 19041) in practice. Activate it at
  setup and refuse per-app audio with the reason where activation fails,
  rather than gating on a build number.

## Related

- [Windows.Graphics.Capture](/quest/m3/capture-wgc.md) - display and window capture with the cursor, which this quest no longer owns
- [A capture listing a picker can draw](/quest/m3/capture-list.md) - lists apps on Windows and gives them the identity an app capture would name
- [Linux capture parity](/quest/m3/capture-linux.md) - the same gaps, through
  the portal and PipeWire
