// Exercises the installed package end to end over a real QUIC session: connect, subscribe,
// read a frame through a future, a continuation, and (on C++20) a coroutine, cancel pending
// reads, and observe errors as returned values. Built with exceptions and RTTI disabled.

#include <moq/moq.hpp>

#include <chrono>
#include <condition_variable>
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
    std::fprintf(stderr, "%s failed: moq::Error variant %zu\n", what, error.get_variant().index());
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

} // namespace

int main() {
    // A synchronous error is a returned value, not an exception.
    auto unbound = moq::Server::init();
    auto fingerprints = unbound->cert_fingerprints();
    CHECK(!fingerprints);
    CHECK(std::holds_alternative<moq::Error::kBind>(fingerprints.error().get_variant()));

    // Publisher: a server whose origin serves one broadcast with one track.
    auto origin = moq::OriginProducer::init({});
    auto broadcast = ok(origin->create_broadcast("probe"), "create_broadcast");
    auto track = ok(broadcast->publish_track("data", std::nullopt), "publish_track");
    ok(broadcast->announce({}), "announce");

    auto server = moq::Server::init();
    ok(server->set_bind("127.0.0.1:0"), "set_bind");
    ok(server->set_tls_generate({"localhost"}), "set_tls_generate");
    ok(server->set_publish(origin), "set_publish");
    auto addr = ok(server->listen().get(), "listen");

    // Both halves of the handshake are futures, so they run concurrently while this
    // thread blocks on one at a time.
    auto client = moq::Client::init();
    ok(client->set_tls_verify(false), "set_tls_verify");
    auto accepting = server->accept();
    auto connecting = client->connect("https://" + addr);
    auto request = ok(accepting.get(), "accept");
    CHECK(request != nullptr);
    auto served = ok(request->accept().get(), "request accept");
    auto session = ok(connecting.get(), "connect");

    // Subscriber: resolve the announced broadcast and subscribe to its track.
    auto announced = ok(session->consume()->announced_broadcast("probe"), "announced_broadcast");
    auto remote = ok(announced->available().get(), "available");
    auto consumer = ok(remote->subscribe_track("data", std::nullopt).get(), "subscribe_track");

    // Cancel a read that has nothing to deliver yet.
    auto pending = consumer->read_frame();
    CHECK(pending.wait_for(50ms) == std::future_status::timeout);
    pending.cancel();
    CHECK(!pending.valid());

    // The consumer survives the cancelled read and delivers the next frame.
    auto reading = consumer->read_frame();
    ok(track->write_frame({bytes("hello"), 1000}), "write_frame");
    auto frame = ok(reading.get(), "read_frame");
    CHECK(frame.has_value());
    CHECK(frame->payload == bytes("hello"));
    CHECK(frame->timestamp_us == 1000);

    // A continuation receives the next frame on the executor.
    {
        Latch done;
        std::optional<moq::Frame> received;
        auto continuation = consumer->read_frame().then(moq::inline_executor, [&](moq::expected<std::optional<moq::Frame>> result) {
            received = ok(std::move(result), "then read_frame");
            done.set();
        });
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

    auto closed = moq::Client::init();
    closed->cancel();
    auto refused = closed->connect("https://" + addr).get();
    CHECK(!refused);
    CHECK(std::holds_alternative<moq::Error::kCancelled>(refused.error().get_variant()));

    ok(broadcast->finish(), "broadcast finish");
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
