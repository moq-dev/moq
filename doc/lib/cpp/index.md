---
title: C++
description: RAII objects, futures, and moq::expected over the Rust core
---

# C++

[![GitHub release](https://img.shields.io/github/v/release/moq-dev/moq?filter=cpp-v*\&label=cpp)](https://github.com/moq-dev/moq/releases?q=cpp-v)

`<moq/moq.hpp>`: every `moq-ffi` object as a `std::shared_ptr`, every async
call as a cancellable future, and every error as a returned `moq::expected`
value, so it builds with exceptions and RTTI disabled (Unreal's defaults).
C++17 is the floor; C++20 adds `co_await` on a future, and C++23 makes
`moq::expected` a `std::expected`. The bindings are generated from the same
[UniFFI](https://mozilla.github.io/uniffi-rs/) crate as the other wrappers, so
a method means the same thing here as in Python or Go. For a plain C ABI, use
[libmoq](/lib/c/).

## Install

Each [`cpp-v*` release](https://github.com/moq-dev/moq/releases?q=cpp-v) ships
`moq-cpp-<version>-<target>.tar.gz` (`.zip` on Windows) holding the `moq-ffi`
static library, the headers, the generated bindings source, a CMake package,
and `moq.pc`. Targets: Linux x86\_64 and aarch64, macOS arm64, Windows x64.
No Rust toolchain is needed to consume it.

```cmake ignore
find_package(moq REQUIRED)   # with CMAKE_PREFIX_PATH at the unpacked archive
target_link_libraries(app PRIVATE moq::moq)
```

```bash
export PKG_CONFIG_PATH="moq-cpp-$ver-$target/lib/pkgconfig"
c++ -std=c++17 app.cpp $(pkg-config --variable=sources moq) $(pkg-config --cflags --libs moq) -o app
```

The generated bindings ship as source (`share/moq/moq.cpp`) and compile inside
your build, because `uniffi::expected` is `std::expected` or a bundled
`tl::expected` depending on the standard. Compile it with the same standard
as the code that includes `<moq/moq.hpp>`; `find_package` does this for you,
and a mismatch fails to link rather than corrupting memory. On MSVC, link the
release runtime (`/MD`), which the Rust library uses in every configuration.

From source, `add_subdirectory(cpp/moq)` in a checkout builds `moq-ffi` with
cargo and renders the bindings with the pinned `uniffi-bindgen-cpp` (see
[`cpp/moq`](https://github.com/moq-dev/moq/tree/main/cpp/moq)), then exposes
the same `moq::moq` target.

## Example

```cpp
#include <moq/moq.hpp>

// Subscribe. Every fallible call returns moq::expected; nothing throws.
auto client = moq::Client::init();
auto session = client->connect("https://relay.example.com").get();
if (!session) {
    report(session.error());   // a moq::Error value
    return;
}

auto announced = (*session)->consume()->announced_broadcast("my-stream.hang");
auto broadcast = (*announced)->available().get();
auto catalogs = (*broadcast)->subscribe_catalog().get();
auto catalog = (*catalogs)->next().get();
if (catalog && *catalog) {
    for (const auto &[name, video] : (*catalog)->video) {
        auto media = (*broadcast)->subscribe_media(name, video.container, std::nullopt).get();
        auto frame = (*media)->next().get();   // std::nullopt once the track ends
    }
}
```

```cpp
// Publish encoded frames, or raw pixels with the codec inside the binding.
// opus_init, packet, and rgba come from your encoder or capture source.
auto broadcast = (*session)->publish()->create_broadcast("my-stream.hang");
auto audio = (*broadcast)->publish_audio({moq::AudioFormat::kOpus, opus_init});
(void)(*audio)->write_frame({packet, 20'000});

moq::VideoEncoderOutput output{moq::VideoCodec::kH264, "camera", std::nullopt, std::nullopt, moq::VideoEncoderKind::kAuto{}};
auto video = (*broadcast)->encode_video({moq::VideoPixelFormat::kRgba, 1280, 720, 30}, output, nullptr);
(void)(*video)->write({0, rgba});
(void)(*broadcast)->announce({});
(void)(*broadcast)->finish();   // keep the producer alive while publishing, then finish explicitly
```

## Futures

An async call returns `moq::Future<T>`, which delivers a `moq::expected<T>`:

- **Block** with `get()`, or bound the wait with `wait_for(timeout)`.
- **Continue** with `std::move(future).then(executor, callback)`. The returned
  `moq::Continuation` owns the call: destroying it cancels.
- **Await** with `co_await` on C++20. The coroutine resumes on the thread that
  completed the future. Destroying a coroutine suspended on a future cancels
  the call, and it never resumes.
- **Cancel** by calling `cancel()` or destroying the future. The Rust future is
  dropped at its next await point; the object it ran on stays usable, so the
  next call works. A cancelled future's continuation never runs.

```cpp
auto reading = (*media)->next();
auto continuation = std::move(reading).then(moq::inline_executor, [](moq::expected<std::optional<moq::MediaFrame>> frame) {
    // Runs on the executor thread: hand the frame off, never block here.
});
```

```cpp ignore
// Inside a coroutine; the result is still a moq::expected.
auto frame = co_await (*media)->next();
```

## Executor

Futures are polled, and continuations run, on one process-wide executor
thread unless you install your own with `moq::set_executor(executor,
shutdown)` before the first async call. An `Executor` takes a `moq::Task` and
returns true once it has accepted it; `shutdown` stops accepting and returns
once no task can still run. A host with its own threads (a game engine, OBS)
installs one that hops onto them.

Never block the executor. A continuation or resumed coroutine that calls
`get()` on another future waits for a poll that only the executor can run,
which deadlocks. `moq::inline_executor` runs a continuation right where the
future completed, which is the executor thread.

## Shutdown

`moq::shutdown()` stops the `moq-ffi` runtime thread, then the executor.
Pending calls resolve `Cancelled`, and every object stays safe to drop. Call
it once before a plugin or engine module unloads, from a thread that is not
running a continuation, so no thread is left calling into unmapped code. A
process that exits normally can skip it.

## Errors

`moq::Error` holds a `std::variant` of cases (`moq::Error::kCancelled`,
`kUnauthorized`, `kProtocol`, ...); test one with
`std::holds_alternative<moq::Error::kUnauthorized>(error.get_variant())`. Auth
rejections are their own cases, so you don't retry them. A Rust panic or a
misused future (`get()` twice) aborts with a message on stderr instead of
throwing.

Everything else maps one to one onto the
[shared feature list](/lib/#what-every-binding-can-do): each generated
`moq::MoqFoo` is also `moq::Foo`, and each Rust method keeps its name. The
header is the reference; every method carries its doc comment.

- Source: [`cpp/moq`](https://github.com/moq-dev/moq/tree/main/cpp/moq); `just cpp check` builds, installs, and tests it locally
