---
title: C++
description: RAII objects, futures, and moq::expected over the Rust core
---

# C++

[![GitHub release](https://img.shields.io/github/v/release/moq-dev/moq?filter=cpp-v*\&label=cpp)](https://github.com/moq-dev/moq/releases?q=cpp-v)

`<moq/moq.hpp>`: every `moq-ffi` object as a `std::shared_ptr`, every async
call as a cancellable future, and every error as a returned `moq::expected`
value, so it builds with exceptions and RTTI disabled (Unreal's defaults).
C++17 is the floor, and C++20 adds `co_await` on a future. The bindings are
generated from the same [UniFFI](https://mozilla.github.io/uniffi-rs/) crate as
the other wrappers, so
a method means the same thing here as in Python or Go. For a plain C ABI, use
[moq-c](/lib/c/).

## Install

Each [`cpp-v*` release](https://github.com/moq-dev/moq/releases?q=cpp-v) ships
`moq-cpp-<version>-<target>.tar.gz` (`.zip` on Windows) holding the `moq-ffi`
static library, the headers, the generated bindings source, a CMake package,
and `moq-cpp.pc`. Targets: Linux x86\_64 and aarch64, macOS arm64, Windows x64.
No Rust toolchain is needed to consume it.

```cmake ignore
find_package(moq-cpp REQUIRED)   # with CMAKE_PREFIX_PATH at the unpacked archive
target_link_libraries(app PRIVATE moq::cpp)
```

```bash
export PKG_CONFIG_PATH="moq-cpp-$ver-$target/lib/pkgconfig"
c++ -std=c++17 app.cpp $(pkg-config --variable=sources moq-cpp) $(pkg-config --cflags --libs moq-cpp) -o app
```

The Windows archive is built for MSVC, so use the CMake package there.

The generated bindings ship as source (`share/moq-cpp/moq.cpp`) and compile inside
your build, with your compiler and flags. On MSVC, link the release runtime
(`/MD`), which the Rust library uses in every configuration.

From source, `add_subdirectory(cpp/moq)` in a checkout builds `moq-ffi` with
cargo and renders the bindings with the pinned `uniffi-bindgen-cpp` (see
[`cpp/moq`](https://github.com/moq-dev/moq/tree/main/cpp/moq)), then exposes
the same `moq::cpp` target. To build from an explicitly staged default-feature
FFI package instead, set `MOQ_FFI_DIR` to its extracted directory, or pass
`--ffi-dir DIR` to `just cpp package`. This renders bindings from the supplied
metadata library and links its static library without compiling Rust. Without
an explicit directory, a release pin in `cpp/moq/FFI_VERSION` selects the default
input; `just cpp package` fetches it with the checked-in SHA-256 hashes and fails
if it cannot verify it. Direct CMake builds and `just cpp check` build the
current checkout unless given an explicit native directory. The package is
`moq-cpp`. It and the C package, `moq-c`, each link in their own copy of the
Rust core, so a program links one of the two, never both.

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
    for (const auto &[id, video] : catalog->video) {
        // The rendition carries its own broadcast and track, which the subscribe follows.
        auto media = ok(moq::MediaTrackConsumer::subscribe(broadcast, {moq::Rendition::kVideo{id, video}}).get());
        auto frame = ok(media->next().get());   // std::nullopt once the track ends
    }
}
```

A session advertises its implementation in SETUP, which a relay's auth service sees.
The config is generated, so nothing names this package for you: set
`ClientConfig::implementation` (and `ServerConfig::implementation`) to `moq::IMPLEMENTATION`,
after any product tokens of your own, and moq-ffi appends its own.

Call `available()` again to wait for the next publisher instance at that path, such as a
restarted publisher, and drop what was read from the previous one then: its tracks end
when its session does.

`MediaTrackConsumer` reads `MediaRead` records in decode order: `kFrame`, `kGap`, or
`kEnd`, like the audio and video decoders. Match the variant before taking its frame.
A gap breaks continuity. An encoder end is not stream EOF: following frames before its
endpoint are the old encoder's drain; the first at or past it starts fresh. For arrival-order reads, decode each group
from `TrackConsumer::recv_group` with `moq::MediaGroupConsumer::init(group, container)`;
frames carry container media time as they land.
Share `moq::MediaTimebase::init(broadcast)` through `ContainerInit` when several imports
use one source timeline; omitting it keeps an independent offset. A timebase belongs to
its broadcast and does not keep it open. `MediaContainerProducer` takes chunks split
anywhere, so a raw byte stream needs no other importer.
`moq::ContainerFormat::kFmp4` splits each fragment into one frame per sample.

```cpp
// Publish encoded frames, or raw pixels with the codec inside the binding.
// session is connected as above; opus_init, packet, and rgba come from your
// encoder or capture source.
auto publication = ok(session->publish()->create_broadcast("my-stream.hang"));
auto broadcast = publication->broadcast(); // fill tracks here; announce on the publication
auto audio = ok(moq::MediaTrackProducer::audio(broadcast, moq::MediaTarget::kNamed{}, {moq::AudioFormat::kOpus, opus_init}));
ok(audio->write_frame({packet, 20'000}));

moq::VideoEncoderOutput output{moq::VideoCodec::kH264, std::nullopt, std::nullopt, moq::VideoEncoderKind::kAuto{}};
auto catalog = ok(moq::MediaCatalogProducer::init(broadcast));
auto video = ok(moq::VideoProducer::init(catalog, {{moq::VideoPixelFormat::kRgba, 1280, 720, {30000, 1001}}, output, "camera", nullptr}));
ok(video->write({0, rgba}));
moq::AnnounceRoute route;
route.epoch = moq::mint_epoch(); // a fresh epoch per run
ok(publication->announce(route));
ok(broadcast->close());    // keep the producer alive while publishing, then close explicitly
```

Raw video encoders take an exact numerator/denominator frame rate, such as 30000/1001. Producer options hold input, codec settings, track naming, and optional session bandwidth ownership; standalone encoders take only codec settings. For simulcast, give each rendition the next one down as `lower`: it stops encoding, advertised as disabled, while its bandwidth grant cannot fund it, and resumes once the grant recovers. A rung only disables while the session probes for bandwidth, which on native transports takes moq-quic with BBR3.

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
auto continuation = std::move(reading).then(moq::inline_executor, [](moq::expected<std::optional<moq::MediaRead>> read) {
    // Runs on the executor thread: match the record and hand it off, never block here.
});
```

```cpp ignore
// Inside a coroutine; the result is still a moq::expected.
auto frame = co_await media->next();
```

## Runtime

Native I/O and background tasks run on a Tokio worker pool. Futures are still
polled on the [executor](#executor), while `get()` and `wait_for()` block the
calling thread; independent native wake callbacks can run concurrently. The
executor passed to `then()` dispatches only the continuation. Call
`moq::shutdown()` from a host thread before unloading the module; it joins
native workers and then the executor. Never call shutdown from a continuation.

## Executor

Futures are polled on one process-wide executor thread unless you install your
own with `moq::set_executor(executor, shutdown)` before the first async call.
An `Executor` takes a `moq::Task` and
returns true once it has accepted it; `shutdown` stops accepting and returns
once no task can still run. A host with its own threads (a game engine)
installs one that hops onto them. The executor passed to `then()` is separate:
the [OBS plugin](/bin/obs) gives each output and source a thread of its own
there, so one slow continuation never delays another.

Never block the executor. A continuation or resumed coroutine that calls
`get()` on another future waits for a poll that only the executor can run,
which deadlocks. `moq::inline_executor` runs a continuation right where the
future completed, which is the executor thread.

## Disabled renditions

Audio and video convenience decoders reject a catalog configuration with `enabled: false`. Select an enabled rendition from the latest catalog before opening a decoder. Among the video renditions you can decode, prefer the highest `preference`; `0` is the default and `-1` marks a compatibility transcode.

## Bandwidth estimates

Hold the owned `BandwidthConsumer` returned by `session->recv_bandwidth()` to receive bandwidth estimates. `rate()` reads the current estimate in bits per second and `rate_changed()` waits for a new one. Releasing the last consumer stops PROBE; the session and stats snapshots do not request it. Client consumers survive reconnects and report no estimate while disconnected.

## Compressed tracks

`moq::TrackInfo::compress` on a raw track sends each group DEFLATE-compressed to opt-in `moq-lite-07-wip` peers; other peers, and every reader here, get the original frames.

## Track demand

`demand()->wanted()` is the group subscribers want, or `std::nullopt` while nobody subscribes, and `wanted_changed()` waits for it to move: start a new group once it passes your largest. Set `TrackInfo::refresh` to have a joining subscriber ask for a new group once the newest has grown past `refresh` times its first frame.

Obtain a dynamic track request’s `demand()` before accepting it to watch subscribers through the handoff to its producer. The watch does not keep the track alive and ends when the track closes or the request is rejected.

## Authorization

On `moq-lite-07-wip` (opt in with `ClientConfig::versions`) and on moq-transport draft-18+ with the [MoQ Auth extension](/draft/moq-auth), `session->grants()` returns a `GrantConsumer`: `grant()` is the `Grant` the relay sent back, empty until it answers and while a client session reconnects, and `changed()` waits for the next one until `cancel()`.

Set `ClientConfig::tokens` instead of putting the credential in the URL. The first is the connection credential, sent in SETUP and, while an offered version can't carry it there (lite before 07), as the URL's `?jwt=`; each other token widens the grant over AUTH. `session->tokens()->set(...)` replaces them before they expire: the next dial presents them, a live AUTH session at once, and after an authorization refusal the session redials only if they changed. `answered()` resolves once the relay accepts them, or to an `Error` carrying its refusal code. A server-accepted session's `tokens()` is null. A `?jwt=` already in the URL stays the connection credential, which is how to reach a relay older than this release.

## Stats

`session->stats()` reports the transport counters, each a `std::optional` that is empty when the transport does not report it, plus the negotiated protocol in `version`. Connect returns a connecting session. Read `session->state()` and wait with `state_changed(state.revision)` to observe its first-dial failures and reconnects; independent watchers keep their own revisions.

## Shutdown

`session->shutdown()` requests a drain immediately; `session->closed().get()` waits up to one second and reports its result.
Application delivery requires explicitly selecting `moq-lite-07-wip` on both
peers; older Lite versions (including default lite-06) and IETF wait only for
transport ACKs. Finish or abort live tracks first.

To end a broadcast cleanly, finish every track, then call `catalog->finish()` on
its `moq::MediaCatalogProducer` to finish the catalog, which ends the content.
The broadcast stays announced and keeps serving the finished tracks, so a
subscriber still catching up can drain them, until `broadcast->close()` takes it
offline. How long to keep it up is yours to decide. A track still open at the
close, or released without a finish, ends with `Unroutable`, which readers take
as the publisher leaving.

`moq::shutdown()` stops the `moq-ffi` runtime workers, then the executor.
Pending calls resolve `Cancelled`, and every object stays safe to drop. Call
it once before a plugin or engine module unloads, from a thread that is not
running a continuation, so no thread is left calling into unmapped code. A
process that exits normally can skip it.

## Errors

Error cases carrying a diagnostic expose it as `reason`; protocol failures expose `details`.
`moq::Error` holds a `std::variant` of cases (`moq::Error::kCancelled`,
`kUnauthorized`, `kProtocol`, ...); test one with
`std::holds_alternative<moq::Error::kUnauthorized>(error.get_variant())`. Auth
rejections are their own cases, so you don't retry them. `error.to_string()`
gives the message Rust's `Display` does, for logs; branch on the case, not the
text. A Rust panic inside a call, or a misused future (`get()` twice), aborts
with a message on stderr instead of throwing. A panic in owned background work reaches the next call on that object as a task error;
other objects keep running.

A session closed for exceeding the relay's request caps reports the protocol
kind `ProtocolKind::kTooManyRequests`. Reduce
the simultaneous announcements or subscriptions, or ask the relay operator
to adjust the [session limits](/bin/relay/config#session-and-peer), before
reconnecting with the same workload.
A session closed because its credential expired reports `ProtocolKind::kExpired`, and a request refused for an expired token is `moq::Error::kExpired`; refresh the token before retrying.
A catalog that needs a newer moq-hang draft than this build reads is `moq::Error::kUnsupportedCatalog`; upgrade the library to read it.

Check a result before reading it. `moq::expected` is a bundled `tl::expected`
at every standard, with the same members as `std::expected`. With exceptions
off, `value()` on an error aborts with a message, while `*result` on an error
is undefined.

Everything else maps one to one onto the
[shared feature list](/lib/#what-every-binding-can-do): moq-ffi's `MoqFoo` is
`moq::Foo`, and each Rust method keeps its name.
[Layers](/lib/layers) maps each wrapped Rust layer to its `moq::` types. The
header is the reference; every method carries its doc comment.

- Source: [`cpp/moq`](https://github.com/moq-dev/moq/tree/main/cpp/moq); `just cpp check` builds, installs, and tests it locally
