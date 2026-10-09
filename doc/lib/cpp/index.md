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
[moq-c](/lib/c/).

## Install

Each [`cpp-v*` release](https://github.com/moq-dev/moq/releases?q=cpp-v) ships
`moq-cpp-<version>-<target>.tar.gz` (`.zip` on Windows) holding the `moq-ffi`
static library, the headers, the generated bindings source, a CMake package,
and `moq-cpp.pc`. Targets: Linux x86\_64 and aarch64, macOS arm64, Windows x64.
No Rust toolchain is needed to consume it.

```cmake ignore
set(CMAKE_CXX_STANDARD 23)       # optional; see "C++ standard" below
find_package(moq-cpp REQUIRED)   # with CMAKE_PREFIX_PATH at the unpacked archive
target_link_libraries(app PRIVATE moq::cpp)
```

```bash
export PKG_CONFIG_PATH="moq-cpp-$ver-$target/lib/pkgconfig"
c++ -std=c++17 app.cpp $(pkg-config --variable=sources moq-cpp) $(pkg-config --cflags --libs moq-cpp) -o app
```

The Windows archive is built for MSVC, so use the CMake package there.

The generated bindings ship as source (`share/moq-cpp/moq.cpp`) and compile inside
your build, because `uniffi::expected` is `std::expected` or a bundled
`tl::expected` depending on the standard. On MSVC, link the release runtime
(`/MD`), which the Rust library uses in every configuration.

### C++ standard

The bindings compile at your project's `CMAKE_CXX_STANDARD`, or the compiler's
default (at least C++17) when it is unset, and must match the code that
includes `<moq/moq.hpp>`. To use C++23, set it before `find_package`, or pass
`-DCMAKE_CXX_STANDARD=23`. Raising the standard only on your own target
(`target_compile_features(app PRIVATE cxx_std_23)`) does not reach the
bindings, so the two disagree on `moq::expected` and the link fails with an
undefined `moq_abi_std_expected` or `moq_abi_tl_expected` symbol rather than
corrupting memory. With pkg-config, pass the same `-std` to the bindings
source and to your code.

From source, `add_subdirectory(cpp/moq)` in a checkout builds `moq-ffi` with
cargo and renders the bindings with the pinned `uniffi-bindgen-cpp` (see
[`cpp/moq`](https://github.com/moq-dev/moq/tree/main/cpp/moq)), then exposes
the same `moq::cpp` target. The package is `moq-cpp`, so it installs beside
the C package, `moq-c`, without colliding.

## Example

Every fallible call returns `moq::expected`, and nothing throws. The samples
unwrap results with this helper, which stops the program on an error; a real
app branches on the error instead (see [Errors](#errors)).

```cpp ignore
#include <moq/moq.hpp>
#include <cstdio>
#include <cstdlib>

// Unwraps a result, or prints the error and aborts.
template <typename T>
T ok(moq::expected<T> result) {
    if (!result) {
        std::fprintf(stderr, "moq: %s\n", result.error().to_string().c_str());
        std::abort();
    }
    return std::move(*result);
}

void ok(moq::expected<void> result) {
    if (!result) {
        std::fprintf(stderr, "moq: %s\n", result.error().to_string().c_str());
        std::abort();
    }
}
```

```cpp
// Subscribe.
auto client = ok(moq::Client::init({}));   // a moq::ClientConfig; {} takes every default
auto session = ok(client->connect("https://relay.example.com").get());
auto announced = ok(session->consume()->announced_broadcast("my-stream.hang"));
auto broadcast = ok(announced->available().get());
auto catalogs = ok(moq::MediaCatalogConsumer::subscribe(broadcast).get());
if (auto catalog = ok(catalogs->next().get())) {   // std::nullopt once the catalog ends
    for (const auto &[name, video] : catalog->video) {
        auto media = ok(moq::MediaContainerConsumer::subscribe(broadcast, {name, video.container}).get());
        auto frame = ok(media->next().get());   // std::nullopt once the track ends
    }
}
```

```cpp
// Publish encoded frames, or raw pixels with the codec inside the binding.
// session is connected as above; opus_init, packet, and rgba come from your
// encoder or capture source.
auto broadcast = ok(session->publish()->create_broadcast("my-stream.hang"));
auto audio = ok(moq::MediaTrackProducer::audio(broadcast, moq::MediaTarget::kNamed{}, {moq::AudioFormat::kOpus, opus_init}));
ok(audio->write_frame({packet, 20'000}));

moq::VideoEncoderOutput output{moq::VideoCodec::kH264, "camera", std::nullopt, std::nullopt, moq::VideoEncoderKind::kAuto{}};
auto video = ok(broadcast->encode_video({moq::VideoPixelFormat::kRgba, 1280, 720, 30}, output, nullptr));
ok(video->write({0, rgba}));
moq::Route route;
route.epoch = moq::mint_epoch(); // a fresh epoch per run
ok(broadcast->announce(route));
ok(broadcast->close());    // keep the producer alive while publishing, then close explicitly
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
- **Check** `valid()` before reading a future that may be cancelled, consumed, or
  moved from. It is false once `get()`, `then()`, `cancel()`, or a move took the
  future's state, and reading an invalid future aborts.

```cpp
auto reading = media->next();
auto continuation = std::move(reading).then(moq::inline_executor, [](moq::expected<std::optional<moq::MediaFrame>> frame) {
    // Runs on the executor thread: hand the frame off, never block here.
});
```

```cpp ignore
// Inside a coroutine; the result is still a moq::expected.
auto frame = co_await media->next();
```

## Executor

Futures are polled, and continuations run, on one process-wide executor
thread unless you install your own with `moq::set_executor(executor,
shutdown)` before the first async call. An `Executor` takes a `moq::Task` and
returns true once it has accepted it; `shutdown` stops accepting and returns
once no task can still run. A host with its own threads (a game engine)
installs one that hops onto them. The executor passed to `then()` is separate:
the [OBS plugin](/bin/obs) gives each output and source a thread of its own
there, so one slow continuation never delays another.

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
rejections are their own cases, so you don't retry them. `error.to_string()`
gives the message Rust's `Display` does, for logs; branch on the case, not the
text. A Rust panic or a misused future (`get()` twice) aborts with a message on
stderr instead of throwing.

Check a result before reading it. With exceptions off below C++23, reading the
value of an error is undefined, through `*result` and `value()` alike, since
the bundled `tl::expected` has nothing to throw.

Everything else maps one to one onto the
[shared feature list](/lib/#what-every-binding-can-do): each generated
`moq::MoqFoo` is also `moq::Foo`, and each Rust method keeps its name. The
header is the reference; every method carries its doc comment.

- Source: [`cpp/moq`](https://github.com/moq-dev/moq/tree/main/cpp/moq); `just cpp check` builds, installs, and tests it locally
