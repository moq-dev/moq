# [M] Validate VAAPI encoding

## Goal

`moq-video`'s VAAPI backend is proven on real hardware and stops being opt-in
for the reason it is opt-in today.

## Plan

The backend is in-tree and its own comment says NOT YET VALIDATED ON HARDWARE,
which is why the `vaapi` feature is off by default. Run it on Intel and AMD
graphics, fix what it finds, and either turn the feature on or record what
still blocks it.

Independent of the V4L2-M2M work: different vendor, different code path,
different hardware to test on. It covers Intel-based ground robots and NUC
companions, which is why it belongs to teleoperation rather than sitting
unowned.
