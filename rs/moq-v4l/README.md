# moq-v4l

Safe Rust bindings for the kernel's Video4Linux 2 API, vendored for the MoQ
workspace. `moq-video` uses them for camera capture and, behind its `v4l2`
feature, the memory-to-memory hardware codecs most ARM SoCs expose.

This is a fork of [`v4l`](https://github.com/raymanfx/libv4l-rs) (MIT,
Copyright Christopher N. Hesse) with one change: the `videodev2.h` bindings
that upstream's `v4l2-sys-mit` generates in a build script are checked in
under `src/sys/`, so a build needs neither libclang nor the kernel headers.
The `libv4l` backend did not survive the fork; every call is an ioctl on the
device node and nothing links at build time.

The bindings are arch-independent: layout tests are off, integer types are
fixed-width, and `timeval` / `timespec` come from `libc`. Regenerate them on a
Linux host with `bindgen-cli` and the kernel UAPI headers installed by running
`src/sys/bindgen.sh`.

## License

MIT, inherited from upstream. See `LICENSE`.
