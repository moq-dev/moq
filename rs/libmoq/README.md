# libmoq

`libmoq` is now [`moq-c`](https://crates.io/crates/moq-c), and this final
release contains no code.

The C API is unchanged: the header is still `moq.h` and the library is still
`libmoq.a` (`moq.lib` on Windows). Switch the crate to `moq-c`, CMake to
`find_package(moq-c)` with the `moq-c::moq` target, and pkg-config to `moq-c`.
Release archives are tagged `moq-c-v*` on
[GitHub](https://github.com/moq-dev/moq/releases?q=moq-c).
