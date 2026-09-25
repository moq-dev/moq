// Exercises the generated C++ bindings end to end over a real QUIC session: connect,
// subscribe, read a frame through a future, cancel a pending read, and observe errors
// as returned values. Built with exceptions and RTTI disabled.

#include <moq.hpp>

#include <chrono>
#include <cstdio>
#include <cstdlib>
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

[[noreturn]] void fail(const char *what, const moq::MoqError &error) {
    std::fprintf(stderr, "%s failed: MoqError variant %zu\n", what, error.get_variant().index());
    std::abort();
}

// Unwraps a result the probe expects to succeed.
template <typename T>
T ok(uniffi::expected<T, moq::MoqError> result, const char *what) {
    if (!result) {
        fail(what, result.error());
    }
    return std::move(*result);
}

void ok(uniffi::expected<void, moq::MoqError> result, const char *what) {
    if (!result) {
        fail(what, result.error());
    }
}

std::vector<uint8_t> bytes(const std::string &text) {
    return std::vector<uint8_t>(text.begin(), text.end());
}

} // namespace

int main() {
    // A synchronous error is a returned value, not an exception.
    auto unbound = moq::MoqServer::init();
    auto fingerprints = unbound->cert_fingerprints();
    CHECK(!fingerprints);
    CHECK(std::holds_alternative<moq::MoqError::kBind>(fingerprints.error().get_variant()));

    // Publisher: a server whose origin serves one broadcast with one track.
    auto origin = moq::MoqOriginProducer::init({});
    auto broadcast = ok(origin->create_broadcast("probe"), "create_broadcast");
    auto track = ok(broadcast->publish_track("data", std::nullopt), "publish_track");
    ok(broadcast->announce({}), "announce");

    auto server = moq::MoqServer::init();
    ok(server->set_bind("127.0.0.1:0"), "set_bind");
    ok(server->set_tls_generate({"localhost"}), "set_tls_generate");
    ok(server->set_publish(origin), "set_publish");
    auto addr = ok(server->listen().get(), "listen");

    // Both halves of the handshake are futures, so they run concurrently while this
    // thread blocks on one at a time.
    auto client = moq::MoqClient::init();
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

    // An async error arrives through the future as a returned value too: this track
    // finished, so reading past its end on a second, finished consumer reports it.
    ok(track->finish(), "track finish");
    auto ended = consumer->read_frame().get();
    CHECK(ended && !ended->has_value());

    auto closed = moq::MoqClient::init();
    closed->cancel();
    auto refused = closed->connect("https://" + addr).get();
    CHECK(!refused);
    CHECK(std::holds_alternative<moq::MoqError::kCancelled>(refused.error().get_variant()));

    ok(broadcast->finish(), "broadcast finish");
    session->cancel(0);
    served->cancel(0);
    server->cancel();

    // Stop dispatching continuations before the process tears down.
    uniffi::shutdown_async_dispatcher();

    std::printf("probe: ok\n");
    return 0;
}
