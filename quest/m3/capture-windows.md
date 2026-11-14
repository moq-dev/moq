# [M] Windows capture parity

## Goal

App capture and system audio work on Windows. Display and window capture,
including the cursor, already run on the Windows.Graphics.Capture backend
(`rs/moq-video/src/capture/wgc/native.rs`), which
[its own quest](/quest/m3/capture-wgc.md) verifies on real hardware.

## Plan

- **Applications** (every window of a process, including ones that open later)
  are still `Unsupported` outside macOS, where `SCShareableContent` gives it
  almost free. Windows has no direct equivalent, so this needs per-window
  composition or an explicit decision that Windows offers window capture only.
  Decide that rather than leaving a variant that errors at runtime.
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

WGC does not answer app capture. It captures one monitor or one window per
item and has no per-process source, so the per-window-composition or
window-only decision still stands now that the backend has landed.

## Waiting

- Maintainer: whether app capture exists off macOS, here and in [Linux capture parity](/quest/m3/capture-linux.md). (A) Windows and Linux offer window capture only: `Source::App` stays macOS-only and fails with its reason elsewhere, and a listing off macOS uses apps only to group windows. (B) Build app capture by composing a process tree's windows. I recommend A, since no consumer asks for B and it matches the capture listing's rule that a listed app must not hand out a source that fails at open.

## Related

- [Windows.Graphics.Capture](/quest/m3/capture-wgc.md) - display and window capture with the cursor, which this quest no longer owns
- [A capture listing a picker can draw](/quest/m3/capture-list.md) - lists apps on Windows and gives them the identity an app capture would name
- [Linux capture parity](/quest/m3/capture-linux.md) - the same gaps, through
  the portal and PipeWire
