// Exercises the installed package end to end over a real QUIC session: connect, subscribe,
// read a frame through a future, a continuation, and (on C++20) a coroutine, cancel pending
// reads, and observe errors as returned values. Built with exceptions and RTTI disabled.

#include <moq/moq.hpp>

#include <chrono>
#include <cmath>
#include <condition_variable>
#include <cstring>
#include <cstdio>
#include <cstdlib>
#include <mutex>
#include <optional>
#include <string>
#include <variant>
#include <vector>

using namespace std::chrono_literals;

// Unlike assert, this survives a release build.
#define CHECK(expr)                                                                        \
    do {                                                                                   \
        if (!(expr)) {                                                                     \
            std::fprintf(stderr, "%s:%d: CHECK failed: %s\n", __FILE__, __LINE__, #expr); \
            std::abort();                                                                  \
        }                                                                                  \
    } while (0)

namespace {

[[noreturn]] void fail(const char *what, const moq::Error &error) {
    std::fprintf(stderr, "%s failed: %s\n", what, error.to_string().c_str());
    std::abort();
}

// Unwraps a result the probe expects to succeed.
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

std::vector<uint8_t> bytes(const std::string &text) {
    return std::vector<uint8_t>(text.begin(), text.end());
}

// Blocks main until a continuation or coroutine on the executor thread reports back.
class Latch {
public:
    // Notifies under the lock: the waiter may destroy the latch as soon as it sees done_.
    void set() {
        std::lock_guard<std::mutex> guard(mutex_);
        done_ = true;
        ready_.notify_all();
    }

    bool wait_for(std::chrono::milliseconds timeout) {
        std::unique_lock<std::mutex> lock(mutex_);
        return ready_.wait_for(lock, timeout, [this] { return done_; });
    }

private:
    std::mutex mutex_;
    std::condition_variable ready_;
    bool done_ = false;
};

#ifdef MOQ_COROUTINES
// Just enough of a coroutine type to start one from main. It frees itself when it finishes,
// and main destroys one that is still suspended.
struct Coroutine {
    struct promise_type {
        Coroutine get_return_object() noexcept {
            return Coroutine{std::coroutine_handle<promise_type>::from_promise(*this)};
        }
        std::suspend_never initial_suspend() noexcept { return {}; }
        std::suspend_never final_suspend() noexcept { return {}; }
        void return_void() noexcept {}
        void unhandled_exception() noexcept { std::abort(); }
    };

    std::coroutine_handle<promise_type> handle;
};

Coroutine read_one(std::shared_ptr<moq::TrackConsumer> consumer, std::optional<moq::Frame> &out, Latch &done) {
    auto frame = co_await consumer->read_frame();
    out = ok(std::move(frame), "co_await read_frame");
    done.set();
}
#endif

// Exercise the installed audio publisher and consumer rather than only generated signatures.
void audio_roundtrip() {
    auto broadcast = ok(moq::BroadcastProducer::init(), "audio broadcast");
    auto receiver = ok(broadcast->consume(), "audio receiver");
    auto catalogs = ok(moq::MediaCatalogConsumer::subscribe(receiver).get(), "audio catalog consumer");
    auto catalog = ok(moq::MediaCatalogProducer::init(broadcast), "audio catalog producer");
    moq::AudioEncoderInput input{moq::AudioSampleFormat::kF32, 48000, 2};
    moq::AudioEncoderOutput output{};
    output.codec = moq::AudioCodec::opus();
    output.frame_duration = std::chrono::milliseconds(20);
    auto audio = ok(moq::AudioProducer::init(catalog, {input, output, "tone", nullptr}), "audio producer");
    auto snapshot = ok(catalogs->next().get(), "audio catalog");
    CHECK(snapshot.has_value());
    moq::AudioDecoderOutput decoded{};
    decoded.format = moq::AudioSampleFormat::kF32;
    // The track's whole retention: reading starts only after finish, and the budget ages
    // groups by wall clock, so a stalled host would otherwise skip the start.
    decoded.max_delay = std::chrono::seconds(30);
    auto consumer = ok(moq::AudioConsumer::subscribe(receiver, "tone", snapshot->audio.at("tone"), decoded).get(), "audio consumer");
    for (size_t packet = 0; packet < 8; packet++) {
        std::vector<uint8_t> data(960 * 2 * sizeof(float));
        for (size_t sample = 0; sample < 960; sample++) {
            for (size_t channel = 0; channel < 2; channel++) {
                float value = 0.4f * std::sin(2.0 * 3.141592653589793 * (channel == 0 ? 400.0 : 1000.0) *
                                           (packet * 960 + sample) / 48000.0);
                std::memcpy(data.data() + (sample * 2 + channel) * sizeof(float), &value, sizeof(value));
            }
        }
        ok(audio->write({250000 + packet * 20000, data}), "write audio");
    }
    ok(audio->finish(), "finish audio");
    size_t frames = 0;
    double real[2][2] = {}, imag[2][2] = {};
    for (;;) {
        auto read = ok(consumer->next().get(), "decoded audio");
        if (!read) break;
        CHECK(!std::holds_alternative<moq::AudioRead::kGap>(read->get_variant()));
        // The finish ends the encoder where its eight 20 ms frames stop.
        if (const auto *end = std::get_if<moq::AudioRead::kEnd>(&read->get_variant())) {
            CHECK(end->end_us == 250000 + 8 * 20000);
            continue;
        }
        const auto *variant = std::get_if<moq::AudioRead::kFrame>(&read->get_variant());
        CHECK(variant != nullptr);

        const auto *next = &variant->frame;

        CHECK(next->timestamp_us == 250000 + frames * 1000000 / 48000);
        CHECK(next->data.size() % (2 * sizeof(float)) == 0);
        for (size_t sample = 0; sample < next->data.size() / (2 * sizeof(float)); sample++) {
            for (size_t channel = 0; channel < 2; channel++) {
                float value;
                std::memcpy(&value, next->data.data() + (sample * 2 + channel) * sizeof(float), sizeof(value));
                for (size_t tone = 0; tone < 2; tone++) {
                    double phase = 2.0 * 3.141592653589793 * (tone == 0 ? 400.0 : 1000.0) * (frames + sample) / 48000.0;
                    real[channel][tone] += value * std::cos(phase);
                    imag[channel][tone] += value * std::sin(phase);
                }
            }
        }
        frames += next->data.size() / (2 * sizeof(float));
    }
    CHECK(frames == 8 * 960);
    for (size_t channel = 0; channel < 2; channel++) {
        auto power = [&](size_t tone) { return real[channel][tone] * real[channel][tone] + imag[channel][tone] * imag[channel][tone]; };
        CHECK(power(channel) > power(1 - channel));
    }
    CHECK(!audio->write({0, std::vector<uint8_t>(8)}));
    ok(broadcast->close(), "close audio broadcast");
}

} // namespace

int main() {
    audio_roundtrip();
    auto timebase_owner = ok(moq::BroadcastProducer::init(), "timebase owner");
    auto timebase_other = ok(moq::BroadcastProducer::init(), "timebase other");
    auto timebase = ok(moq::MediaTimebase::init(timebase_owner), "timebase");
    auto foreign = moq::MediaContainerProducer::init(timebase_other, {moq::ContainerFormat::kFmp4, {}, timebase});
    CHECK(!foreign);
    CHECK(std::holds_alternative<moq::Error::kCodec>(foreign.error().get_variant()));
    ok(timebase_owner->close(), "close timebase owner");
    CHECK(!moq::MediaTimebase::init(timebase_owner));
    ok(timebase_other->close(), "close timebase other");

    // A finished catalog leaves the broadcast up, still serving its finished tracks, until it closes.
    auto finished = ok(moq::BroadcastProducer::init(), "finished broadcast");
    auto finished_catalog = ok(moq::MediaCatalogProducer::init(finished), "finished catalog");
    ok(ok(finished->publish_track("status", {}), "publish track")->finish(), "finish track");
    ok(finished_catalog->finish(), "catalog finish");
    ok(finished->consume(), "consume a finished broadcast");
    ok(finished->close(), "close finished broadcast");
    CHECK(!finished_catalog->finish());

    // A synchronous error is a returned value, not an exception.
    auto unbound = ok(moq::Server::init({}), "server init");
    auto fingerprints = unbound->cert_fingerprints();
    CHECK(!fingerprints);
    CHECK(std::holds_alternative<moq::Error::kBind>(fingerprints.error().get_variant()));
    CHECK(std::get<moq::Error::kBind>(fingerprints.error().get_variant()).reason ==
          "not listening; call listen() first");
    // It prints the message Rust's Display gives.
    CHECK(fingerprints.error().to_string() == "bind: not listening; call listen() first");

    // Publisher: a server whose origin serves one broadcast with one track. Dropping the
    // publication keeps the route, which lasts until the broadcast closes.
    auto origin = moq::OriginProducer::init({});
    std::shared_ptr<moq::BroadcastProducer> broadcast;
    std::shared_ptr<moq::TrackProducer> track;
    {
        auto publication = ok(origin->create_broadcast("probe"), "create_broadcast");
        broadcast = publication->broadcast();
        moq::TrackInfo info;
        info.timescale = 1000000;
        track = ok(broadcast->publish_track("data", info), "publish_track");
        ok(publication->announce({}), "announce");
    }

    // A closed broadcast's publication can no longer announce.
    auto stale = ok(origin->create_broadcast("stale"), "create_broadcast");
    ok(stale->broadcast()->close(), "close stale broadcast");
    auto reannounced = stale->announce({});
    CHECK(!reannounced);
    CHECK(std::holds_alternative<moq::Error::kClosed>(reannounced.error().get_variant()));

    moq::ServerConfig server_config;
    server_config.bind = "127.0.0.1:0";
    server_config.versions = {"moq-lite-07-wip"};
    server_config.tls.generate = {"localhost"};
    server_config.publish = origin;
    auto server = ok(moq::Server::init(server_config), "server init");
    auto addr = ok(server->listen().get(), "listen");

    // Both halves of the handshake are futures, so they run concurrently while this
    // thread blocks on one at a time.
    moq::ClientConfig client_config;
    client_config.tls.insecure = true;
    client_config.versions = server_config.versions;
    client_config.implementation = moq::IMPLEMENTATION;
    auto client = ok(moq::Client::init(client_config), "client init");
    auto accepting = server->accept();
    auto connecting = client->connect("https://" + addr);
    auto request = ok(accepting.get(), "accept");
    CHECK(request != nullptr);
    // The package names itself ahead of moq-ffi, so a server can tell bindings apart.
    auto implementation = request->peer_implementation();
    CHECK(implementation && implementation->rfind("moq-cpp/", 0) == 0);
    auto served = ok(request->accept(nullptr, nullptr).get(), "request accept");
    auto session = ok(connecting.get(), "connect");

    // Subscriber: resolve the announced broadcast and subscribe to its track.
    auto announced = ok(session->consume()->follow("probe", std::nullopt), "follow");
    auto remote = ok(announced->available().get(), "available");
    auto consumer = ok(remote->subscribe_track("data", std::nullopt).get(), "subscribe_track");

    // Cancel a read that has nothing to deliver yet.
    auto pending = consumer->read_frame();
    CHECK(pending.wait_for(50ms) == std::future_status::timeout);
    CHECK(pending.valid());
    pending.cancel();
    CHECK(!pending.valid());

    // A move takes the state, so the source is invalid and the destination is not.
    {
        auto source = consumer->read_frame();
        CHECK(source.valid());
        auto destination = std::move(source);
        CHECK(!source.valid());
        CHECK(destination.valid());
    }

    // The consumer survives the cancelled read and delivers the next frame.
    auto reading = consumer->read_frame();
    ok(track->write_frame({bytes("hello"), 1000}), "write_frame");
    auto frame = ok(reading.get(), "read_frame");
    // get() consumed the state.
    CHECK(!reading.valid());
    CHECK(frame.has_value());
    CHECK(frame->payload == bytes("hello"));
    // Lite-07 carries both the Implementation parameter and frame timestamps.
    CHECK(frame->timestamp_us == 1000);

    // A continuation receives the next frame on the executor.
    {
        Latch done;
        std::optional<moq::Frame> received;
        auto reading_next = consumer->read_frame();
        auto continuation = std::move(reading_next).then(moq::inline_executor, [&](moq::expected<std::optional<moq::Frame>> result) {
            received = ok(std::move(result), "then read_frame");
            done.set();
        });
        // then() took the future's state.
        CHECK(!reading_next.valid());
        ok(track->write_frame({bytes("then"), 2000}), "write_frame");
        CHECK(done.wait_for(5s));
        CHECK(received && received->payload == bytes("then"));
    }

#ifdef MOQ_COROUTINES
    // Destroying a coroutine suspended on a read cancels the read, and it never resumes.
    {
        Latch done;
        std::optional<moq::Frame> received;
        auto abandoned = read_one(consumer, received, done);
        CHECK(!done.wait_for(50ms));
        abandoned.handle.destroy();
    }

    // A coroutine resumes with the next frame.
    {
        Latch done;
        std::optional<moq::Frame> received;
        auto reader = read_one(consumer, received, done);
        ok(track->write_frame({bytes("co_await"), 3000}), "write_frame");
        CHECK(done.wait_for(5s));
        CHECK(received && received->payload == bytes("co_await"));
        (void)reader;
    }
#endif

    // An async error arrives through the future as a returned value too: this track
    // finished, so reading past its end reports it.
    ok(track->finish(), "track finish");
    auto ended = consumer->read_frame().get();
    CHECK(ended && !ended->has_value());

    auto closed = ok(moq::Client::init({}), "client init");
    closed->cancel();
    auto refused = closed->connect("https://" + addr).get();
    CHECK(!refused);
    CHECK(std::holds_alternative<moq::Error::kCancelled>(refused.error().get_variant()));

    ok(broadcast->close(), "broadcast close");
    session->cancel(0);
    served->cancel(0);
    server->cancel();

    // Stop the runtime and the executor before the process tears down. The handles above
    // stay safe to drop afterwards.
    moq::shutdown();

#ifdef MOQ_COROUTINES
    std::printf("probe: ok (with coroutines)\n");
#else
    std::printf("probe: ok\n");
#endif
    return 0;
}
