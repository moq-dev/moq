# [S] moq-video: a reused X11 window id can publish an unrelated window

## Goal

X11 window capture stops when the selected window is destroyed, even if the X
server hands the same id to a replacement window.

## Plan

`capture/x11.rs` identifies its target by XID alone and revalidates it each
frame with `get_geometry`. A destroyed window makes that request fail, which
correctly ends the stream. The gap is XID reuse: X clients allocate ids from
their own range and do reuse freed ones, so a window destroyed and replaced by
the same client can inherit the id. If the replacement's even-clamped
dimensions match, capture continues against it and publishes a window the user
never selected.

The exposure is narrow, since all of that has to happen inside one frame
interval, but the failure mode is publishing content nobody chose, which is the
same class of problem as the layered-window leak fixed during
[#3244](https://github.com/moq-dev/moq/pull/3244) review.

`StructureNotify` is the fix: select for it on the target at open and drain
events before each frame, so `DestroyNotify` ends the stream regardless of what
happens to the id afterwards. Event masks are per-client, so this does not
disturb the window's owner, and it is a read-only interest rather than the
`SetPropW` marker the Windows backend writes into a foreign window.
`ConfigureNotify` arrives on the same subscription and could later replace the
per-frame `get_geometry` round trip.

Found by Codex while reviewing #3244. Needs a real X session to verify.

## Related

- [X11 capture transport](/quest/m2/x11-capture-shm.md) - would consume the same event subscription to drop the per-frame geometry poll
