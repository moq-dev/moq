// The same in-process rounds as the other binding benchmark runners (see bench/README.md).
#include <moq/moq.hpp>

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <ctime>
#include <filesystem>
#include <fstream>
#include <future>
#include <string>
#include <vector>

namespace {
using Clock = std::chrono::steady_clock;
constexpr int warmup = 3;

template <typename T> T ok(moq::expected<T> result) {
    if (!result) {
        std::fprintf(stderr, "%s\n", result.error().to_string().c_str());
        std::abort();
    }
    return std::move(*result);
}

void ok(moq::expected<void> result) {
    if (!result) {
        std::fprintf(stderr, "%s\n", result.error().to_string().c_str());
        std::abort();
    }
}

void require(bool value) {
    if (!value) {
        std::fputs("missing, reordered, or corrupt frame\n", stderr);
        std::abort();
    }
}

struct Case {
    int size, publishers, subscribers;
    int frames() const { return std::clamp((8 << 20) / size, 1, 64); }
    double delivered() const { return double(publishers) * subscribers * frames() * size; }
    std::string name() const {
        return "size=" + std::to_string(size) + "/pub=" + std::to_string(publishers) +
               "/sub=" + std::to_string(subscribers);
    }
};

double seconds(Clock::time_point start) {
    return std::chrono::duration<double>(Clock::now() - start).count();
}

double cpu() {
    auto ticks = std::clock();
    require(ticks != std::clock_t(-1));
    return double(ticks) / CLOCKS_PER_SEC;
}

struct Result {
    std::string name, unit;
    double value;
};

// Each reader is a coroutine on the binding's executor, like the other runners'
// tasks. Spawning a new OS thread for each group would measure thread creation.
struct Reader {
    std::future<void> completion;
    struct promise_type {
        std::promise<void> completion;
        Reader get_return_object() { return {completion.get_future()}; }
        std::suspend_never initial_suspend() noexcept { return {}; }
        std::suspend_never final_suspend() noexcept { return {}; }
        void return_void() { completion.set_value(); }
        void unhandled_exception() noexcept { std::abort(); }
    };
};

Reader read(Case c, std::shared_ptr<moq::TrackConsumer> consumer,
            const std::vector<uint8_t> &payload, uint64_t n) {
    auto group = ok(co_await consumer->recv_group());
    require(group != nullptr);
    for (int f = 0; f < c.frames(); ++f) {
        auto frame = ok(co_await group->read_frame());
        require(frame && frame->payload == payload && frame->timestamp_us == n * c.frames() + f);
    }
}

void throughput(Case c, double budget, std::vector<Result> &results) {
    auto origin = moq::OriginProducer::init({uint64_t(64 << 20)});
    auto broadcast = ok(origin->create_broadcast("bench"))->broadcast();
    std::vector<std::shared_ptr<moq::TrackProducer>> tracks;
    std::vector<std::shared_ptr<moq::TrackConsumer>> consumers;
    moq::TrackInfo info;
    info.timescale = 1000000;
    for (int p = 0; p < c.publishers; ++p) {
        auto track = ok(broadcast->publish_track(std::to_string(p), info));
        for (int s = 0; s < c.subscribers; ++s)
            consumers.push_back(ok(track->consume({})));
        tracks.push_back(std::move(track));
    }
    std::vector<uint8_t> payload(c.size);
    for (int i = 0; i < c.size; ++i)
        payload[i] = i % 251;
    auto step = [&](uint64_t n) {
        std::vector<std::future<void>> readers;
        for (auto &consumer : consumers) {
            readers.push_back(read(c, consumer, payload, n).completion);
        }
        for (auto &track : tracks) {
            auto group = ok(track->append_group());
            for (int f = 0; f < c.frames(); ++f)
                ok(group->write_frame({payload, n * c.frames() + f}));
            ok(group->finish());
        }
        for (auto &reader : readers) {
            if (reader.wait_for(std::chrono::seconds(30)) != std::future_status::ready) {
                std::fputs("binding benchmark round did not complete\n", stderr);
                std::abort();
            }
            reader.get();
        }
    };
    for (int n = 0; n < warmup; ++n)
        step(n);
    auto start = Clock::now();
    auto cpu_start = cpu();
    uint64_t n = warmup;
    do {
        step(n++);
    } while (seconds(start) < budget);
    auto wall = seconds(start);
    auto cpu_used = cpu() - cpu_start;
    auto mb = (n - warmup) * c.delivered() / 1e6;
    results.push_back({"throughput/" + c.name(), "MB/s", mb / wall});
    results.push_back({"cpu/" + c.name(), "ms/MB", cpu_used * 1e3 / mb});
    ok(broadcast->close());
}
} // namespace

int main() {
#ifdef _WIN32
    std::fputs("C++ process CPU benchmark requires Linux or macOS\n", stderr);
    return 1;
#endif
    const auto *out = std::getenv("MOQ_BENCH_DIR");
    if (!out) {
        std::fputs("MOQ_BENCH_DIR is required\n", stderr);
        return 1;
    }
    const auto *setting = std::getenv("MOQ_BENCH_SECONDS");
    if (!setting)
        setting = "1";
    char *end;
    double budget = std::strtod(setting, &end);
    if (end == setting || *end || !std::isfinite(budget) || budget <= 0) {
        std::fputs("MOQ_BENCH_SECONDS must be positive finite seconds\n", stderr);
        return 1;
    }
    std::vector<Result> results;
    for (auto c : {Case{100, 1, 1},
                   {1024, 1, 1},
                   {64 * 1024, 1, 1},
                   {1024 * 1024, 1, 1},
                   {1024, 1, 4},
                   {1024, 4, 1},
                   {1024, 4, 4}})
        throughput(c, budget, results);

    auto broadcast = ok(moq::BroadcastProducer::init());
    auto track = ok(broadcast->publish_track("calls", {}));
    auto producer = ok(track->append_group());
    auto group = ok(producer->consume());
    auto consumer = ok(track->consume({}));
    auto call = [&](const char *name, auto fn) {
        auto start = Clock::now();
        uint64_t calls = 0;
        do {
            for (int i = 0; i < 1000; ++i)
                fn();
            calls += 1000;
        } while (seconds(start) < budget);
        results.push_back({std::string("call/") + name, "ns", seconds(start) * 1e9 / calls});
    };
    call("sequence", [&] { group->sequence(); });
    call("info", [&] { (void)consumer->info(); });
    std::filesystem::create_directories(out);
    std::ofstream file(std::filesystem::path(out) / "cpp.json");
    file.exceptions(std::ios::failbit | std::ios::badbit);
    file << "[\n";
    for (size_t i = 0; i < results.size(); ++i) {
        const auto &r = results[i];
        file << (i ? ",\n" : "") << "{\"name\":\"" << r.name << "\",\"unit\":\"" << r.unit
             << "\",\"value\":" << r.value << "}";
    }
    file << "\n]\n";
    moq::shutdown();
}
