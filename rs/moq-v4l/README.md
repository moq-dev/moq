# moq-v4l

Safe Rust bindings for the kernel's Video4Linux 2 API, vendored for the MoQ
workspace. `moq-video` uses them for camera capture and, behind its `v4l2`
feature, the memory-to-memory hardware codecs most ARM SoCs expose.

This is a fork of [`v4l`](https://github.com/raymanfx/libv4l-rs) (MIT,
Copyright Christopher N. Hesse). The `videodev2.h` bindings
that upstream's `v4l2-sys-mit` generates in a build script are checked in
under `src/sys/`, so a build needs neither libclang nor the kernel headers.
The `libv4l` backend did not survive the fork; every call is an ioctl on the
device node and nothing links at build time.
The unused high-level control API is removed; `moq-video` sets scalar controls
through the raw `sys` and `v4l2` layers instead.

The bindings are arch-independent: layout tests are off, integer types are
fixed-width, and `timeval` / `timespec` come from `libc`. Regenerate them on a
Linux host with `bindgen-cli` and the kernel UAPI headers installed by running
`src/sys/bindgen.sh`.

## Upstream

Vendored from tag `v4l-0.14.0`
([`867d9b12`](https://github.com/raymanfx/libv4l-rs/commit/867d9b122fde603ff7f1bfcdf19de48845c9e1f7)).
Last checked on 2026-10-31 through upstream `master` at
[`30f6dbae`](https://github.com/raymanfx/libv4l-rs/commit/30f6dbaeed5cdb1b33760fc5b35c1b545cfe0e46)
(2024-10-05), including all 19 commits after the tag. Upstream has no
[published security advisories](https://github.com/raymanfx/libv4l-rs/security/advisories)
as of that check.

| Upstream change | Disposition |
| --- | --- |
| [`0394443c`](https://github.com/raymanfx/libv4l-rs/commit/0394443c51571181f7aa77d5d6e015b27914b730), retain unknown flags | Ported across every remaining flag conversion, with a test covering every bit and mixed known/unknown values. |
| [`b7a50013`](https://github.com/raymanfx/libv4l-rs/commit/b7a500133f09f2c648577b99ff0c82a99cbd6be5), scalar extended-control size; [`6d171419`](https://github.com/raymanfx/libv4l-rs/commit/6d171419c18659b54a6570a1294c047a8d704548), unknown control types; [`dfbb778c`](https://github.com/raymanfx/libv4l-rs/commit/dfbb778c8a9151f74b337eaa432c727a33e9bb9e), control query by description | Removed the unused `control` module and `Device::{query_controls, control, set_control, set_controls}` instead of maintaining them. Raw controls used by moq-video are unaffected. |
| [`8837f57b`](https://github.com/raymanfx/libv4l-rs/commit/8837f57b705999cbab78874aa4a85c4c78bfd438), `VbiCapture` spelling | Ported; the numeric buffer type remains 4. |
| [`26d203a0`](https://github.com/raymanfx/libv4l-rs/commit/26d203a0d7d5924cf88f72690ca514ae0cd2ce87) and merge [`83df458f`](https://github.com/raymanfx/libv4l-rs/commit/83df458f21a4ffaf78ebe4287934dceab4312ee0), bitflags 2 | Already carried by the fork. |
| [`8953d413`](https://github.com/raymanfx/libv4l-rs/commit/8953d413fd3c334df708ae33e20974c842de3ca8), clippy cleanup | The `usize::MAX` change is already carried; the glium example is not vendored. |
| [`33817d4a`](https://github.com/raymanfx/libv4l-rs/commit/33817d4aa5c52f5646f52368acb94113a47a43ab), FreeBSD | Not applicable: this fork and its consumers use the Linux kernel API only. |
| [`7976fed3`](https://github.com/raymanfx/libv4l-rs/commit/7976fed387149846168dc66eda805b59b72a7621), libv4l conversion lints; [`677b02d6`](https://github.com/raymanfx/libv4l-rs/commit/677b02d60278a67f1d7c3916d38a42c16fa64fde), pkg-config dependency | Not applicable: the libv4l backend and sys build scripts are not vendored. |
| [`6a6017bf`](https://github.com/raymanfx/libv4l-rs/commit/6a6017bf9c94f6bf576cdf84605c2adfa0ce7b05), bindgen; [`21d6b20c`](https://github.com/raymanfx/libv4l-rs/commit/21d6b20cf2f7dc0d2b66cc88dcb5f3e9bc90ebf7), jpeg-decoder; [`ced9df0b`](https://github.com/raymanfx/libv4l-rs/commit/ced9df0bb2ab3c1b03783536fceb209a630d23c8), glium | Dependency/example updates skipped; those builds and examples are not vendored. |
| [`c8ec2f07`](https://github.com/raymanfx/libv4l-rs/commit/c8ec2f071d777f83bda57f0e99d129bfc6d76814), [`63755ad4`](https://github.com/raymanfx/libv4l-rs/commit/63755ad417aff5f5b451f92a22e4e95a542d0309), [`c85ecb2f`](https://github.com/raymanfx/libv4l-rs/commit/c85ecb2f542b8307f934b368a0e5966aa5f736c5), CI; [`566e08a0`](https://github.com/raymanfx/libv4l-rs/commit/566e08a0f13f9b0c0b69fdc89f58dd067212af61), README badge; [`30f6dbae`](https://github.com/raymanfx/libv4l-rs/commit/30f6dbaeed5cdb1b33760fc5b35c1b545cfe0e46), changelog links | Not applicable: upstream CI and release documentation are not vendored. |

## License

MIT, inherited from upstream. See `LICENSE`.
