# [M] moq-video: X11 capture copies the framebuffer over the socket and re-polls RandR every frame

## Goal

X11 display capture moves pixels through shared memory rather than the X
connection, and learns about layout changes from RandR events rather than by
asking every frame.

## Plan

`capture/x11.rs` reads pixels with `GetImage`, which serializes the whole
requested rectangle through the X connection. On a local server that is ~8 MB
per frame through a Unix socket at 1080p, before any conversion. MIT-SHM
(`shm_get_image`, which `x11rb` supports) is the standard fast path: attach a
shared segment once at open and let the server write into it directly. Fall back
to `GetImage` when the extension is absent or the display is remote, since SHM
is meaningless there.

Separately, `Capture::read` calls `randr_get_monitors` plus a `get_atom_name`
per monitor on every frame purely to notice a layout change, and allocates a
`String` per monitor each time. RandR's `SCREEN_CHANGE_NOTIFY` is the intended
mechanism: select for it once, then drain pending events between frames. Window
capture does the same thing with a per-frame `get_geometry`; it already selects
`StructureNotify` and drains that queue between frames, so `ConfigureNotify` is
there to be consumed and the round trip dropped.

Both changes are contained to the one backend and are verifiable on a Linux
host with a real X session; CI compiles the file but cannot run it.

## Related

- [Capture frame buffers](/quest/m2/capture-frame-buffers.md) - the per-frame allocations in the same read path
