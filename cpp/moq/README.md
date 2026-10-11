# moq C++ package

C++17 bindings for [rs/moq-ffi](../../rs/moq-ffi): the sources `uniffi-bindgen-cpp` generates, plus `include/moq/moq.hpp`, a thin layer that renames them (`moq::MoqClient` is also `moq::Client`) and adds the executor, shutdown, and `co_await` glue. Nothing here re-implements a method; a shape problem is fixed in moq-ffi, where every binding benefits. User docs: [doc/lib/cpp](../../doc/lib/cpp/index.md).

## Layout

- `CMakeLists.txt` builds moq-ffi as a staticlib with cargo, renders the bindings into the build tree, and compiles them as the `moq::cpp` target. Use it with `add_subdirectory`, or install it for `find_package(moq-cpp)` and `moq-cpp.pc`.
- `src/moq.cpp` compiles the generated `moq/ffi/moq.cpp`. It ships as source so it builds with the consumer's standard and flags.
- `cmake/` holds the installed CMake package and pkg-config templates.
- `build.sh` builds the release archive `release-cpp.yml` publishes.
- `test/probe.cpp` exercises the installed package over a real QUIC session.

## Checking

`just cpp check` builds and installs the package, builds the probe against it with `find_package` at C++17 and C++23 and, outside Windows, with pkg-config at C++17, runs each, compiles the doc samples (also outside Windows), and fails if a generated type lacks its short alias. The probe builds with exceptions and RTTI disabled.

The generator is a fork, [kixelated/uniffi-bindgen-cpp](https://github.com/kixelated/uniffi-bindgen-cpp). It carries LiveKit's async support ported to uniffi 0.32, plus the `error_style = "expected"` option `uniffi.toml` turns on. The dev shell provides it. Without Nix:

```bash
cargo install uniffi-bindgen-cpp --locked \
    --git https://github.com/kixelated/uniffi-bindgen-cpp \
    --tag v0.11.0-kixelated.4+v0.32.2
```

`flake.nix` pins the same tag and lists every other place that names it.

## Shape

Every generated type lives in `namespace moq`. Objects are `std::shared_ptr`; records and enums are values.

- A fallible call returns `moq::expected<T>`, which is `std::expected` on C++23 and a bundled `tl::expected` below it. The generated sources and every includer must agree on which, so `moq/moq.hpp` references a symbol named for the one it sees and `src/moq.cpp` defines the one it was built with: a mismatch fails to link.
- An async call returns `moq::Future<T>`. Block on it with `get()` or `wait_for()`, attach a continuation with `std::move(future).then(executor, callback)`, which returns a `moq::Continuation`, or `co_await` it on C++20.
- `moq::Error` is a value holding a `std::variant` of its cases, and `to_string()` gives Rust's `Display` message. Nothing throws. A Rust panic aborts with a message on stderr, and so does reading an invalid future (see [Cancellation](#cancellation)).
- Futures are polled and continuations run on one process-wide executor thread unless the application installs its own with `moq::set_executor` before the first async call. `moq::shutdown()` stops the moq-ffi runtime thread (`moq_ffi_shutdown`), then the executor, before unloading the code a continuation could call into.

## C++ standard

The installed package compiles `moq.cpp` inside the consumer's project, at its `CMAKE_CXX_STANDARD` (else the compiler's default, at least C++17). To use C++23, set `CMAKE_CXX_STANDARD 23` before `find_package(moq-cpp)`, or pass `-DCMAKE_CXX_STANDARD=23`. `target_compile_features(app PRIVATE cxx_std_23)` alone does not reach the separate `moq-cpp` library, so `<moq/moq.hpp>` and the bindings pick different `expected` types and the link fails on `moq_abi_std_expected` or `moq_abi_tl_expected`. `just cpp check` builds the probe at C++23 both ways.

## Cancellation

Cancelling a future (`cancel()`, destroying it, destroying the `moq::Continuation` returned by `then`, or destroying a coroutine suspended on it) drops the Rust future. Most async calls run in place, on the thread that polls the future, so cancelling stops the work at once: it never finishes unobserved or takes data the next call is waiting for. The few calls spawned onto the moq-ffi runtime are aborted at their next await point. The continuation of a cancelled future never runs.

Like `std::future`, `valid()` is false once `get()`, `then()`, `cancel()`, or a move took the future's state. Reading an invalid future aborts with "it was consumed or cancelled", so check `valid()` first when a future may have been cancelled, consumed, or moved from.

The abort discards the operation, not the handle it ran on, so the next call on that object works. Where a method keeps partial progress, its doc comment says so. For example, a cancelled `read_frame` leaves the current group for the next read. Every write in moq-ffi (`write_frame`, `finish`, `abort`) is synchronous, so cancelling a future can never tear a write. Only the async operations (reads, subscribes, connects, accepts, and `reject`) can be cut short.
