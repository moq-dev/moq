# [M] moq-video: Windows window capture returns black pixels for GPU-composited windows

## Goal

`moq import capture --window <id>` on Windows publishes the window's real
contents for browsers, Electron apps, and UWP apps, not a black rectangle.
Capturing an elevated window either works or fails with a clear permission
error, rather than failing at open because the identity marker could not be
written.

## Plan

`capture/window.rs` snapshots the selected window with `BitBlt` from its window
DC and then asks the target to repaint into our memory DC by sending `WM_PRINT`
cross-process. Neither reaches a window that renders through DirectComposition,
which today is Chrome, Edge, Electron, and every UWP app: the window DC holds
nothing the compositor used, so the copy is black.

`PrintWindow(hwnd, hdc, PW_RENDERFULLCONTENT)` is the supported API for exactly
this. It marshals the DC across the process boundary itself (sending `WM_PRINT`
by hand is not a documented cross-process contract), and `PW_RENDERFULLCONTENT`
(Windows 8.1+) is the flag that makes a GPU-composited window render into the
target DC at all. That is the smallest fix and should come first.

Longer term, evaluate Windows.Graphics.Capture (Windows 10 1803+). WGC is the
modern path: hardware-accelerated, captures occluded windows, handles DPI and
per-window rounded corners, and delivers `IDirect3DSurface` frames that could
feed the existing D3D11 surface path zero-copy instead of the current
BGRA-to-CPU-I420 round trip. It costs a WinRT dependency and a capture-picker
consent story, so it is a separate decision from the `PrintWindow` fix.

While in here, revisit `WindowIdentity`. It calls `SetPropW` on a window owned
by another process to detect `HWND` reuse. UIPI blocks that write for an
elevated window, so `Capture::open` fails outright on a window that plain GDI
capture could have read. Prefer an identity check that only reads: the owning
process id plus its creation time is stable, cheap, and needs no write to a
foreign window.

Verification needs a Windows host: PR CI is Linux-only and `just rs windows`
must run on Windows, so none of this code has ever been compiled by automation.
Check the four cases by hand: a plain Win32 window, a Chrome window, an
elevated window, and a window on a scaled display.

## Related

- [Window lifecycle](/quest/m0/capture-window-lifecycle.md) - the other defect in the same backend
- [Windows capture parity](/quest/m2/capture-windows.md) - where the wider Windows.Graphics.Capture decision lives
