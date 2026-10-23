// The moq C++ package: the generated moq-ffi bindings, plus the executor, shutdown, and
// coroutine glue they need. Every type and method is generated; this header only wires them.
#pragma once

#include <moq/ffi/moq.hpp>

#include <chrono>
#include <functional>
#include <string>
#include <utility>
#include <vector>

#if defined(__cpp_impl_coroutine) && __has_include(<coroutine>)
#include <coroutine>
#include <memory>
#include <mutex>
#include <optional>
#include <thread>
#define MOQ_COROUTINES 1
#endif

// Not a uniffi export, so the generated bindings do not declare it; see moq::shutdown.
extern "C" void moq_ffi_shutdown(void);

namespace moq {

// The value of a fallible call, or the Error that stopped it. tl::expected at every standard.
template <typename T>
using expected = ::uniffi::expected<T, Error>;

// Wraps an Error so it converts to any moq::expected.
using ::uniffi::unexpected;

// A pending async call: block with get() or wait_for(), or attach a continuation with then().
template <typename T>
using Future = ::uniffi::Future<T, Error>;

// Owns a continuation attached with then(); destroying it cancels the call.
using Continuation = ::uniffi::FutureContinuation;

// One unit of work handed to an Executor.
using Task = ::uniffi::AsyncTask;

// Runs a Task somewhere and returns true, or returns false to refuse it.
using Executor = ::uniffi::AsyncDispatcher;

// Runs a continuation on the thread that completed the future, for work that never blocks.
inline bool inline_executor(Task task) {
    task();
    return true;
}

// Replaces the default executor that polls futures, before the first async call.
// `shutdown` stops accepting tasks and returns once none can still run; moq::shutdown calls it.
inline void set_executor(Executor executor, std::function<void()> shutdown) noexcept {
    ::uniffi::set_async_dispatcher(std::move(executor), std::move(shutdown));
}

// Stops the moq-ffi runtime workers, then the executor, before the process or module goes away.
// Pending calls resolve Cancelled. Call it once from a thread that is not running a continuation.
inline void shutdown() noexcept {
    moq_ffi_shutdown();
    ::uniffi::shutdown_async_dispatcher();
}

// Sets the log level: "error", "warn", "info", "debug", "trace", or "". Errors if called twice.
inline expected<void> log_level(const std::string &level) {
    return moq_log_level(level);
}

// The settings a default ClientConfig dials with, for a settings UI to show. Knobs whose default
// depends on the backend (GSO, path MTU discovery, congestion control, system roots) stay unset.
inline ClientConfig client_defaults() {
    return moq_client_defaults();
}

// The protocol versions offered by default, most preferred first, as ClientConfig::versions spells them.
inline std::vector<std::string> versions() {
    return moq_versions();
}

// Whether this build can write qlog traces for QuicConfig::qlog.
inline bool qlog_supported() {
    return moq_qlog_supported();
}

// Mints a fresh publisher epoch, ordered newest last. Announce one per run in AnnounceRoute::epoch,
// so viewers see a restart as a new broadcast instead of a stalled one.
inline std::string mint_epoch() {
    return moq_mint_epoch();
}

// The wall-clock time an epoch encodes, to the millisecond. Errors unless it is a UUIDv7.
// Millisecond ticks hold every 48-bit UUIDv7 time, where system_clock's nanoseconds overflow in 2262.
inline expected<std::chrono::time_point<std::chrono::system_clock, std::chrono::milliseconds>>
epoch_time(const std::string &epoch) {
    auto ms = moq_epoch_time_ms(epoch);
    if (!ms) {
        return unexpected(std::move(ms).error());
    }
    return std::chrono::time_point<std::chrono::system_clock, std::chrono::milliseconds>(
        std::chrono::milliseconds(static_cast<std::chrono::milliseconds::rep>(*ms)));
}

#ifdef MOQ_COROUTINES
namespace detail {

// Resumes a coroutine with a future's result. Completion and cancellation share
// `state_`. The continuation resumes a claimed handle before it drops the mutex,
// so the destructor cannot free the frame between the claim and the resume.
template <typename T>
class Awaiter {
public:
    using Output = typename Future<T>::Output;

    explicit Awaiter(Future<T> future) noexcept:
        future_(std::move(future)), state_(std::make_shared<State>()) {}

    Awaiter(const Awaiter &) = delete;
    Awaiter &operator=(const Awaiter &) = delete;

    // Destroying a suspended coroutine cancels the call, and it never resumes.
    ~Awaiter() {
        std::optional<Continuation> continuation;
        {
            // Recursive: resume() calls this on the completing thread while that
            // thread still holds the mutex inside the continuation.
            std::lock_guard<std::recursive_mutex> guard(state_->mutex);
            if (state_->resuming && state_->resume_on == std::this_thread::get_id()) {
                return;
            }
            state_->destroying = true;
            state_->handle = nullptr;
            continuation = std::move(state_->continuation);
        }
    }

    bool await_ready() const noexcept {
        return false;
    }

    bool await_suspend(std::coroutine_handle<> handle) noexcept {
        auto continuation = std::move(future_).then(inline_executor, [state = state_](Output output) {
            std::lock_guard<std::recursive_mutex> guard(state->mutex);
            state->output.emplace(std::move(output));
            if (state->destroying || !state->handle) {
                return;
            }
            auto resume = std::exchange(state->handle, nullptr);
            state->resuming = true;
            state->resume_on = std::this_thread::get_id();
            struct Clear {
                State &state;
                ~Clear() {
                    state.resuming = false;
                }
            } clear{*state};
            resume.resume();
        });

        std::lock_guard<std::recursive_mutex> guard(state_->mutex);
        state_->continuation.emplace(std::move(continuation));
        if (state_->output) {
            // Completed before suspending: carry on without a round trip.
            return false;
        }
        state_->handle = handle;
        return true;
    }

    Output await_resume() noexcept {
        std::lock_guard<std::recursive_mutex> guard(state_->mutex);
        return std::move(*state_->output);
    }

private:
    struct State {
        std::recursive_mutex mutex;
        std::coroutine_handle<> handle;
        std::optional<Output> output;
        std::optional<Continuation> continuation;
        bool destroying = false;
        bool resuming = false;
        std::thread::id resume_on{};
    };

    Future<T> future_;
    std::shared_ptr<State> state_;
};

} // namespace detail
#endif

} // namespace moq

#ifdef MOQ_COROUTINES
// Found by argument-dependent lookup on uniffi::Future.
namespace uniffi {

// `co_await future` resumes with the future's result on the thread that completed it.
template <typename T>
moq::detail::Awaiter<T> operator co_await(Future<T, moq::Error> &&future) noexcept {
    return moq::detail::Awaiter<T>(std::move(future));
}

} // namespace uniffi
#endif
