// Cross-language interop client for the interop test, built against the installed cpp/moq
// package with find_package(moq-cpp).
//
// publish reads raw Annex-B H.264 from stdin (e.g. piped from ffmpeg) and feeds it to a
// streaming importer, which infers frame boundaries. Alongside it, a tone per channel
// (--tone) is encoded through libopus so the matrix exercises the FFI audio path, not only
// the video one. subscribe connects, waits for a catalog with video and audio, decodes
// --audio-ms of every audio rendition to --audio-out.<codec> for tone.py to judge, and exits 0
// once a non-empty video frame arrives too (exit 1 on timeout or no data).
//
//   ffmpeg ... -f h264 - | cpp-interop publish --url http://localhost:4443 --broadcast b.hang --tone 440
//   cpp-interop subscribe --url http://localhost:4443 --broadcast b.hang --timeout 20 \
//       --audio-out /tmp/audio --audio-ms 600
//
// --version offers only that version. --grant waits for the grant the relay sends over AUTH
// and prints it as an `auth granted` line before the client starts.

#include <moq/moq.hpp>

#include <atomic>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <optional>
#include <sstream>
#include <string>
#include <thread>
#include <utility>
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

// SubscribeMedia max delay: how much reordering the jitter buffer tolerates.
constexpr std::chrono::nanoseconds MAX_DELAY = std::chrono::seconds(1);

// Synthetic audio: a 48 kHz tone per channel at 0.5 amplitude, encoded as Opus.
constexpr const char *AUDIO_TRACK = "tone";
constexpr uint32_t AUDIO_RATE = 48'000;
constexpr double AUDIO_AMPLITUDE = 0.5;
constexpr double PI = 3.14159265358979323846;
// A non-default frame duration, and the shortest Opus offers. 20 ms would pass even if the
// duration were truncated to milliseconds somewhere.
constexpr std::chrono::nanoseconds AUDIO_FRAME_DURATION = std::chrono::microseconds(2'500);
// Written in 20 ms batches, so each write spans eight encoded Opus frames.
constexpr uint64_t AUDIO_BATCH_US = 20'000;
constexpr size_t AUDIO_BATCH_SAMPLES = AUDIO_RATE * AUDIO_BATCH_US / 1'000'000;

// The open session, which a failure closes so the relay sees a close rather than timing the
// connection out.
std::shared_ptr<moq::Session> open_session;

[[noreturn]] void exit_failed() {
    // Taken first, so a failure while closing cannot close it again.
    if (auto session = std::exchange(open_session, nullptr)) {
        session->shutdown();
        session->closed().wait_for(std::chrono::seconds(5));
    }
    std::exit(1);
}

[[noreturn]] void fail(const char *what, const moq::Error &error) {
    std::fprintf(stderr, "error: %s: %s\n", what, error.to_string().c_str());
    exit_failed();
}

[[noreturn]] void fail(const char *what) {
    std::fprintf(stderr, "error: %s\n", what);
    exit_failed();
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

// A JSON list of strings, the shape the Rust client logs its grant's patterns in.
std::string patterns(const std::vector<std::string> &list) {
    std::string out = "[";
    for (size_t i = 0; i < list.size(); i++) {
        out += (i == 0 ? "\"" : ", \"") + list[i] + "\"";
    }
    return out + "]";
}

// The session flags both roles share.
struct Dial {
    // When set, the only version offered.
    std::string version;
    // Wait for the relay's grant and print it.
    bool grant = false;
};

// The session owns its connection; the client that made it can go. With `grant`, waits for
// the relay's grant and prints it in the Rust client's `auth granted` shape.
std::shared_ptr<moq::Session> connect(const std::string &url, const Dial &dial, Clock::time_point deadline) {
    moq::ClientConfig config;
    config.tls.insecure = true;
    if (!dial.version.empty()) {
        config.versions = {dial.version};
    }
    auto client = ok(moq::Client::init(config), "client init");
    open_session = until(client->connect(url), deadline, "connect");
    if (dial.grant) {
        auto grants = open_session->grants();
        auto grant = grants->grant();
        while (!grant) {
            grant = until(grants->changed(), deadline, "grant");
        }
        std::fprintf(
            stderr, "auth granted publish=%s subscribe=%s\n", patterns(grant->publish).c_str(),
            patterns(grant->subscribe).c_str()
        );
    }
    return open_session;
}

// Closes the session and waits for the close to reach the relay. `cancel` would return at
// once, and stopping the runtime right after it can leave the relay to time the connection out.
void close_session(const std::shared_ptr<moq::Session> &session) {
    open_session = nullptr;
    session->shutdown();
    until(session->closed(), Clock::now() + std::chrono::seconds(5), "session shutdown");
}

// Feeds the encoder a real-time tone per channel, interleaved, until `stop` is set.
void publish_tone(
    const std::shared_ptr<moq::AudioProducer> &audio, const std::vector<double> &tones, const std::atomic<bool> &stop
) {
    const auto started = Clock::now();
    std::vector<uint8_t> data(AUDIO_BATCH_SAMPLES * tones.size() * sizeof(float));
    uint64_t timestamp_us = 0;
    size_t phase = 0;
    while (!stop.load()) {
        for (size_t i = 0; i < AUDIO_BATCH_SAMPLES; i++) {
            for (size_t c = 0; c < tones.size(); c++) {
                auto sample = static_cast<float>(
                    AUDIO_AMPLITUDE * std::sin(2 * PI * tones[c] * static_cast<double>(phase + i) / AUDIO_RATE)
                );
                // Little-endian f32, which is every target this runs on.
                std::memcpy(&data[(i * tones.size() + c) * sizeof(float)], &sample, sizeof(float));
            }
        }
        ok(audio->write({timestamp_us, data}), "tone write");
        phase += AUDIO_BATCH_SAMPLES;
        timestamp_us += AUDIO_BATCH_US;
        // Pace against the start, so encoding cost doesn't accumulate as drift.
        std::this_thread::sleep_until(started + std::chrono::microseconds(timestamp_us));
    }
}

int publish(
    const std::string &url, const std::string &path, const Dial &dial, const std::vector<double> &tones,
    const std::string &finish_ack
) {
    // No deadline to publish: the harness stops the publisher when the round ends.
    const auto forever = Clock::now() + std::chrono::hours(24);
    auto session = connect(url, dial, forever);

    // Hold the producer for the lifetime of the publish loop; close() unpublishes.
    auto publication = ok(session->publish()->create_broadcast(path), "create_broadcast");
    auto broadcast = publication->broadcast();
    auto catalog = ok(moq::MediaCatalogProducer::init(broadcast), "catalog producer");
    auto media = ok(
        moq::MediaTrackStreamProducer::video(broadcast, moq::MediaTarget::kNamed{}, {moq::VideoFormat::kAvc3, {}}),
        "video stream producer"
    );
    moq::AudioEncoderOutput output{moq::AudioCodec::opus()};
    output.frame_duration = AUDIO_FRAME_DURATION;
    auto audio = ok(
        moq::AudioProducer::init(
            catalog,
            moq::AudioProducerOptions{
                {moq::AudioSampleFormat::kF32, AUDIO_RATE, static_cast<uint32_t>(tones.size())},
                output, AUDIO_TRACK, nullptr
            }
        ),
        "audio producer"
    );
    ok(publication->announce({}), "announce");
    std::printf(
        "publishing \"%s\" (Annex-B H.264 from stdin + a %zu-channel tone) to %s\n", path.c_str(), tones.size(),
        url.c_str()
    );
    std::fflush(stdout);

    std::atomic<bool> stop{false};
    std::thread tone([&] { publish_tone(audio, tones, stop); });

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
    if (!finish_ack.empty()) {
        // Finishing the catalog ends the content; the broadcast stays up for the late readers
        // until the close takes it offline.
        ok(catalog->finish(), "catalog finish");
        const auto linger = std::chrono::seconds(10);
        std::printf(
            "finish ready linger_us=%llu\n",
            static_cast<unsigned long long>(std::chrono::microseconds(linger).count())
        );
        std::fflush(stdout);
        std::this_thread::sleep_for(linger);
        ok(broadcast->close(), "broadcast close");
        std::puts("finish ended");
        std::fflush(stdout);
        auto health_publication = ok(session->publish()->create_broadcast(path + ".health"), "health broadcast");
        auto health = health_publication->broadcast();
        auto track = ok(health->publish_track("live", std::nullopt), "health track");
        const std::string payload = "session-alive";
        ok(track->write_frame({std::vector<uint8_t>(payload.begin(), payload.end()), std::nullopt}), "health write");
        ok(track->finish(), "health finish");
        ok(health_publication->announce({}), "health announce");
        // Only teardown waits on the FIFO; linger has already completed on its own clock.
        std::ifstream ack(finish_ack);
        std::string line;
        if (!std::getline(ack, line) || line != "complete" || session->state().connects != 1) {
            fail("finish acknowledgement or session continuity failed");
        }
        ok(health->close(), "health close");
        std::puts("finish acknowledged");
    } else {
        ok(broadcast->close(), "broadcast close");
    }
    close_session(session);
    moq::shutdown();
    return 0;
}

// Decodes the first `audio_ms` of the rendition to `prefix`.<codec>.{txt,pcm}, the layout tone.py
// reads. The codec is the catalog's up to its first `.` (`opus`, `mp4a`); two renditions of one
// codec fail rather than overwrite each other.
void decode_audio(
    const std::shared_ptr<moq::BroadcastConsumer> &consumer, const std::string &id, const moq::Audio &audio,
    const std::string &prefix, uint64_t audio_ms, Clock::time_point deadline
) {
    const std::string out = prefix + "." + audio.codec.substr(0, audio.codec.find('.'));
    if (std::filesystem::exists(out + ".txt")) {
        fail(("a second " + audio.codec + " rendition").c_str());
    }
    moq::AudioDecoderOutput output{};
    output.format = moq::AudioSampleFormat::kF32;
    output.max_delay = MAX_DELAY;
    auto decoder = until(moq::AudioConsumer::subscribe(consumer, id, audio, output), deadline, "audio consumer");

    std::ofstream log(out + ".txt");
    std::ofstream pcm(out + ".pcm", std::ios::binary);
    log << audio.sample_rate << " " << audio.channel_count << "\n";
    const size_t frame_bytes = sizeof(float) * audio.channel_count;
    uint64_t decoded = 0;
    while (decoded * 1000 < audio_ms * audio.sample_rate) {
        auto read = until(decoder->next(), deadline, "audio frame");
        if (!read) {
            fail("audio track ended early");
        }
        // moq-audio reports a hole it saw directly; tone.py checks the timestamps too.
        if (std::holds_alternative<moq::AudioRead::kGap>(read->get_variant())) {
            fail("audio gap");
        }
        const auto *variant = std::get_if<moq::AudioRead::kFrame>(&read->get_variant());
        if (!variant) {
            fail("audio track ended early");
        }
        const auto *frame = &variant->frame;

        const size_t samples = frame->data.size() / frame_bytes;
        log << frame->timestamp_us << " " << samples << "\n";
        pcm.write(reinterpret_cast<const char *>(frame->data.data()), static_cast<std::streamsize>(frame->data.size()));
        decoded += samples;
    }
    decoder->cancel();
    if (!log || !pcm) {
        fail("writing the decoded audio");
    }
}

int subscribe(
    const std::string &url, const std::string &path, const Dial &dial, double timeout,
    const std::string &audio_out, uint64_t audio_ms, bool finish, size_t expected_frames
) {
    const auto deadline =
        Clock::now() + std::chrono::duration_cast<Clock::duration>(std::chrono::duration<double>(timeout));
    auto session = connect(url, dial, deadline);

    std::shared_ptr<moq::AnnounceConsumer> routes;
    if (finish) {
        std::puts("finish reader ready");
        std::fflush(stdout);
        std::string line;
        if (!std::getline(std::cin, line) || line != "read") fail("finish read phase was not released");
        routes = ok(session->consume()->announced({}), "finish announcements");
    }
    const auto read_deadline = finish
        ? Clock::now() + std::chrono::duration_cast<Clock::duration>(std::chrono::duration<double>(timeout))
        : deadline;
    auto announced = ok(session->consume()->announced_broadcast(path), "announced_broadcast");
    auto consumer = until(announced->available(), read_deadline, "available");

    // The catalog is a live track. A lazy publisher (e.g. the browser, which only encodes on
    // demand) may announce a rendition in a later update rather than the first snapshot, so
    // wait for a catalog that has both video and audio. A finished fixture has video only.
    auto catalogs = until(moq::MediaCatalogConsumer::subscribe(consumer), read_deadline, "catalog consumer");
    std::optional<moq::Catalog> catalog;
    while (!catalog || catalog->video.empty() || (!finish && catalog->audio.empty())) {
        catalog = until(catalogs->next(), read_deadline, "catalog");
        if (!catalog) {
            fail("catalog stream ended without a video and an audio track");
        }
    }
    const auto &[id, video] = *catalog->video.begin();

    moq::Subscription subscription;
    subscription.max_delay =
        finish ? std::chrono::duration_cast<std::chrono::nanoseconds>(std::chrono::duration<double>(timeout)) : MAX_DELAY;
    auto media = until(
        moq::MediaTrackConsumer::subscribe(consumer, {moq::Rendition::kVideo{id, video}, subscription}), read_deadline,
        "container consumer"
    );

    // Audio first: it has to arrive without a gap, and the video consumer keeps its first frame
    // (or skips to a newer one) while this reads. One rendition at a time, each from its own
    // subscription, so none waits unread behind another.
    if (!finish) {
        for (const auto &[audio_id, audio] : catalog->audio) {
            decode_audio(consumer, audio_id, audio, audio_out, audio_ms, deadline);
        }
    }

    size_t total = 0, frames = 0;
    while (finish || total == 0) {
        auto read = until(media->next(), read_deadline, "next read");
        if (!read) { break; }
        if (std::holds_alternative<moq::MediaRead::kGap>(read->get_variant())) fail("media gap");
        const auto *frame = std::get_if<moq::MediaRead::kFrame>(&read->get_variant());
        if (!frame) continue; // Encoder ends do not terminate the subscription.
        total += frame->frame.payload.size();
        if (!frame->frame.payload.empty()) ++frames;
    }
    if (total == 0) {
        fail("no frame data received");
    }
    std::printf("received %zu bytes from \"%s\"\n", total, path.c_str());
    if (finish) {
        if (frames != expected_frames) fail("wrong finished media frame count");
        while (until(catalogs->next(), read_deadline, "catalog EOF")) {}
        for (;;) {
            auto event = until(routes->next(), read_deadline, "announcement END");
            if (!event) fail("origin ended before broadcast withdrawal");
            const auto *end = std::get_if<moq::AnnounceEvent::kEnd>(&event->get_variant());
            if (end && end->announce.prefix == path) break;
        }
        auto watch = ok(session->consume()->announced_broadcast(path + ".health"), "health watch");
        auto health = until(watch->available(), read_deadline, "health available");
        auto track = until(health->subscribe_track("live", std::nullopt), read_deadline, "health subscribe");
        auto group = until(track->next_group(), read_deadline, "health group");
        if (!group) fail("missing health group");
        auto frame = until(group->read_frame(), read_deadline, "health frame");
        if (!frame || std::string(frame->payload.begin(), frame->payload.end()) != "session-alive" || frame->timestamp_us) {
            fail("health payload or timestamp");
        }
        if (until(group->read_frame(), read_deadline, "health group EOF") ||
            until(track->next_group(), read_deadline, "health track EOF") || session->state().connects != 1) {
            fail("health EOF or session continuity");
        }
        std::printf("finish clean frames=%zu catalog=eof announce=end health=ok\n", frames);
    }
    close_session(session);
    moq::shutdown();
    return 0;
}

} // namespace

int main(int argc, char **argv) {
    if (argc < 2) {
        fail(
            "usage: cpp-interop publish|subscribe --url URL --broadcast PATH [--timeout SECONDS] [--version V] "
            "[--grant] [--tone HZ,...] [--audio-out PREFIX --audio-ms MS]"
        );
    }
    const std::string role = argv[1];
    std::string url;
    std::string broadcast;
    Dial dial;
    bool finish = false;
    std::string finish_ack;
    size_t frames = 10;
    std::vector<double> tones;
    std::string audio_out;
    uint64_t audio_ms = 0;
    double timeout = 20;
    for (int i = 2; i < argc; i++) {
        const std::string flag = argv[i];
        if (flag == "--finish") {
            finish = true;
            continue;
        }
        if (flag == "--grant") {
            dial.grant = true;
            continue;
        }
        if (i + 1 >= argc) {
            fail(("missing value for " + flag).c_str());
        }
        const std::string value = argv[++i];
        if (flag == "--url") {
            url = value;
        } else if (flag == "--broadcast") {
            broadcast = value;
        } else if (flag == "--timeout") {
            timeout = std::atof(value.c_str());
        } else if (flag == "--finish-ack") {
            finish_ack = value;
        } else if (flag == "--frames") {
            frames = std::strtoull(value.c_str(), nullptr, 10);
            if (frames == 0) fail("frames must be positive");
        } else if (flag == "--version") {
            dial.version = value;
        } else if (flag == "--tone") {
            std::stringstream list(value);
            for (std::string hz; std::getline(list, hz, ',');) {
                tones.push_back(std::atof(hz.c_str()));
            }
        } else if (flag == "--audio-out") {
            audio_out = value;
        } else if (flag == "--audio-ms") {
            audio_ms = std::strtoull(value.c_str(), nullptr, 10);
        } else {
            fail(("unknown flag " + flag).c_str());
        }
    }
    if (url.empty() || broadcast.empty()) {
        fail("--url and --broadcast are required");
    }
    // Warnings and errors from the Rust layer, such as a publication outside the grant.
    ok(moq::log_level("warn"), "log level");

    if (role == "publish") {
        if (tones.empty()) {
            fail("publish needs --tone");
        }
        return publish(url, broadcast, dial, tones, finish_ack);
    }
    if (role == "subscribe") {
        if (!finish && (audio_out.empty() || audio_ms == 0)) {
            fail("subscribe needs --audio-out and --audio-ms");
        }
        return subscribe(url, broadcast, dial, timeout, audio_out, audio_ms, finish, frames);
    }
    fail(("unknown role " + role).c_str());
}
