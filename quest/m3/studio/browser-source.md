# [L] Browser sources

## Goal

A browser source renders a URL (an overlay, alert box, or chat widget) into
the compositor at the canvas frame rate. The creator can interact with it,
and its audio joins the bus. A crash in the browser process drops that
source, never the stream.

## Plan

Decided 2026-10-22: use CEF through `cef-rs`, desktop only.
- **Rejected engines:** Servo's site compatibility is not yet enough for
  common overlay sites. Blitz runs no JavaScript.
- **wasm build:** omits browser sources until a web API can draw HTML into a
  canvas.
- **Rendering:** CEF offscreen rendering with a shared texture
  (`OnAcceleratedPaint`), imported into wgpu as a D3D11 shared handle on
  Windows, an IOSurface on macOS, and a dmabuf on Linux. The CPU `OnPaint`
  path is the fallback and shows in stats.
- **Process:** CEF runs out of process.
- **First check:** whether `cef-rs` exposes accelerated offscreen painting.
  If it does not, upstream that before building on it.

## Required

- [The studio app](/quest/m3/studio/app.md) - the compositor and sources this adds a source type to
