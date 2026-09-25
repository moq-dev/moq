// Cross-language interop client for the interop test, built against the installed cpp/moq
// package with find_package(moq-cpp).
//
// publish reads raw Annex-B H.264 from stdin (e.g. piped from ffmpeg) and feeds it to a
// streaming importer, which infers frame boundaries. Alongside it, a synthetic tone is
// encoded through libopus so the matrix exercises the FFI audio path, not only the video
// one. subscribe connects, finds the video track in the catalog, and exits 0 as soon as any
// non-empty frame arrives (exit 1 on timeout or no data).
//
//   ffmpeg ... -f h264 - | cpp-interop publish --url http://localhost:4443 --broadcast b.hang
//   cpp-interop subscribe --url http://localhost:4443 --broadcast b.hang --timeout 20

#include <moq/moq.hpp>

#include <atomic>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <optional>
#include <string>
#include <thread>
#include <type_traits>
#include <variant>
#include <vector>

#ifdef _WIN32
#include <io.h>
#define read_stdin(buf, len) _read(0, buf, static_cast<unsigned>(len))
#else
#include <unistd.h>
#define read_stdin(buf, len) ::read(0, buf, len)
#endif

namespace {

using Clock = std::chrono::steady_clock;

constexpr size_t READ_CHUNK = 64 * 1024;

// SubscribeMedia max age: how much reordering the jitter buffer tolerates.
constexpr uint64_t MAX_AGE_US = 1'000'000;

// Synthetic audio: a 48 kHz mono tone, encoded as Opus.
constexpr const char *AUDIO_TRACK = "tone";
constexpr uint32_t AUDIO_RATE = 48'000;
constexpr double AUDIO_TONE_HZ = 440.0;
constexpr double PI = 3.14159265358979323846;
// A non-default frame duration, and the shortest Opus offers. 20 ms would pass even if the
// microsecond field were truncated to milliseconds somewhere.
constexpr uint32_t AUDIO_FRAME_DURATION_US = 2'500;
// Written in 20 ms batches, so each write spans eight encoded Opus frames.
constexpr uint64_t AUDIO_BATCH_US = 20'000;
constexpr size_t AUDIO_BATCH_SAMPLES = AUDIO_RATE * AUDIO_BATCH_US / 1'000'000;

template <typename V, typename = void>
struct has_message: std::false_type {};
template <typename V>
struct has_message<V, std::void_t<decltype(std::declval<const V &>().v1)>>: std::true_type {};

// moq::Error has no Display in the bindings yet, so name the variant and its message.
[[noreturn]] void fail(const char *what, const moq::Error &error) {
    std::string message = std::visit(
        [](const auto &variant) -> std::string {
            if constexpr (has_message<std::decay_t<decltype(variant)>>::value) {
                return variant.v1;
            } else {
                return "";
            }
        },
        error.get_variant()
    );
    std::fprintf(stderr, "error: %s: moq::Error variant %zu %s\n", what, error.get_variant().index(), message.c_str());
    std::exit(1);
}

[[noreturn]] void fail(const char *what) {
    std::fprintf(stderr, "error: %s\n", what);
    std::exit(1);
}

template <typename T>
T ok(moq::expected<T> result, const char *what) {
    if (!result) {
        fail(what, result.error());
    }
    return std::move(*result);
}

void ok(moq::expected<void> result, const char *what) {
    if (!result) {
        fail(what, result.error());
    }
}

// Blocks on a future until the deadline; a timeout cancels it and fails the run.
template <typename T>
T until(moq::Future<T> future, Clock::time_point deadline, const char *what) {
    if (future.wait_for(deadline - Clock::now()) != std::future_status::ready) {
        fail((std::string(what) + ": timed out").c_str());
    }
    return ok(future.get(), what);
}

// The session owns its connection; the client that made it can go.
std::shared_ptr<moq::Session> connect(const std::string &url, Clock::time_point deadline) {
    auto client = moq::Client::init();
    ok(client->set_tls_verify(false), "set_tls_verify");
    return until(client->connect(url), deadline, "connect");
}

// Feeds the encoder a real-time tone until `stop` is set.
void publish_tone(const std::shared_ptr<moq::AudioProducer> &audio, const std::atomic<bool> &stop) {
    const auto started = Clock::now();
    std::vector<uint8_t> data(AUDIO_BATCH_SAMPLES * sizeof(float));
    uint64_t timestamp_us = 0;
    size_t phase = 0;
    while (!stop.load()) {
        for (size_t i = 0; i < AUDIO_BATCH_SAMPLES; i++) {
            auto sample = static_cast<float>(std::sin(2 * PI * AUDIO_TONE_HZ * static_cast<double>(phase + i) / AUDIO_RATE));
            // Little-endian f32, which is every target this runs on.
            std::memcpy(&data[i * sizeof(float)], &sample, sizeof(float));
        }
        ok(audio->write({timestamp_us, data}), "tone write");
        phase += AUDIO_BATCH_SAMPLES;
        timestamp_us += AUDIO_BATCH_US;
        // Pace against the start, so encoding cost doesn't accumulate as drift.
        std::this_thread::sleep_until(started + std::chrono::microseconds(timestamp_us));
    }
}

int publish(const std::string &url, const std::string &path) {
    // No deadline to publish: the harness stops the publisher when the round ends.
    const auto forever = Clock::now() + std::chrono::hours(24);
    auto session = connect(url, forever);

    // Hold the producer for the lifetime of the publish loop; finish() unpublishes.
    auto broadcast = ok(session->publish()->create_broadcast(path), "create_broadcast");
    auto media = ok(broadcast->publish_video_stream({moq::VideoFormat::kAvc3, {}}), "publish_video_stream");
    moq::AudioEncoderOutput output{moq::AudioCodec::opus()};
    output.frame_duration_us = AUDIO_FRAME_DURATION_US;
    auto audio = ok(
        broadcast->encode_audio(AUDIO_TRACK, {moq::AudioSampleFormat::kF32, AUDIO_RATE, 1}, output, nullptr),
        "encode_audio"
    );
    ok(broadcast->announce({}), "announce");
    std::printf(
        "publishing \"%s\" (Annex-B H.264 from stdin + a %.0f Hz tone) to %s\n", path.c_str(), AUDIO_TONE_HZ,
        url.c_str()
    );
    std::fflush(stdout);

    std::atomic<bool> stop{false};
    std::thread tone([&] { publish_tone(audio, stop); });

    // read returns as soon as any bytes are available, so ffmpeg's real-time output is
    // forwarded rather than batched into full chunks.
    std::vector<uint8_t> buf(READ_CHUNK);
    for (;;) {
        auto n = read_stdin(buf.data(), buf.size());
        if (n < 0) {
            fail("read stdin");
        }
        if (n == 0) {
            break;
        }
        ok(media->write(std::vector<uint8_t>(buf.begin(), buf.begin() + n)), "video write");
    }

    // Let the tone unwind before finishing, so no write races a finished producer.
    stop.store(true);
    tone.join();
    ok(audio->finish(), "audio finish");
    ok(media->finish(), "video finish");
    ok(broadcast->finish(), "broadcast finish");
    session->cancel(0);
    moq::shutdown();
    return 0;
}

int subscribe(const std::string &url, const std::string &path, double timeout) {
    const auto deadline =
        Clock::now() + std::chrono::duration_cast<Clock::duration>(std::chrono::duration<double>(timeout));
    auto session = connect(url, deadline);

    auto announced = ok(session->consume()->announced_broadcast(path), "announced_broadcast");
    auto consumer = until(announced->available(), deadline, "available");

    // The catalog is a live track. A lazy publisher (e.g. the browser, which only encodes on
    // demand) may announce video in a later update rather than the first snapshot, so wait
    // for a catalog that actually has a video track.
    auto catalogs = until(consumer->subscribe_catalog(), deadline, "subscribe_catalog");
    std::optional<moq::Catalog> catalog;
    while (!catalog || catalog->video.empty()) {
        catalog = until(catalogs->next(), deadline, "catalog");
        if (!catalog) {
            fail("catalog stream ended without a video track");
        }
    }
    const auto &[name, video] = *catalog->video.begin();

    moq::Subscription subscription;
    subscription.max_age_us = MAX_AGE_US;
    auto media = until(consumer->subscribe_media(name, video.container, subscription), deadline, "subscribe_media");

    size_t total = 0;
    while (total == 0) {
        auto frame = until(media->next(), deadline, "next frame");
        if (!frame) {
            break;
        }
        total += frame->payload.size();
    }
    if (total == 0) {
        fail("no frame data received");
    }
    std::printf("received %zu bytes from \"%s\"\n", total, path.c_str());
    session->cancel(0);
    moq::shutdown();
    return 0;
}

} // namespace

int main(int argc, char **argv) {
    if (argc < 2) {
        fail("usage: cpp-interop publish|subscribe --url URL --broadcast PATH [--timeout SECONDS]");
    }
    const std::string role = argv[1];
    std::string url;
    std::string broadcast;
    double timeout = 20;
    for (int i = 2; i + 1 < argc; i += 2) {
        const std::string flag = argv[i];
        if (flag == "--url") {
            url = argv[i + 1];
        } else if (flag == "--broadcast") {
            broadcast = argv[i + 1];
        } else if (flag == "--timeout") {
            timeout = std::atof(argv[i + 1]);
        } else {
            fail(("unknown flag " + flag).c_str());
        }
    }
    if (url.empty() || broadcast.empty()) {
        fail("--url and --broadcast are required");
    }

    if (role == "publish") {
        return publish(url, broadcast);
    }
    if (role == "subscribe") {
        return subscribe(url, broadcast, timeout);
    }
    fail(("unknown role " + role).c_str());
}
