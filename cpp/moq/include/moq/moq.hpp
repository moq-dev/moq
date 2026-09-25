// The moq C++ package: the generated moq-ffi bindings under short names, plus the
// executor, shutdown, and coroutine glue they need. Every type and method is generated;
// this header only renames and wires them.
#pragma once

#include <moq/ffi/moq.hpp>

#include <functional>
#include <string>
#include <utility>

#if defined(__cpp_impl_coroutine) && __has_include(<coroutine>)
#include <coroutine>
#include <memory>
#include <mutex>
#include <optional>
#define MOQ_COROUTINES 1
#endif

// Not a uniffi export, so the generated bindings do not declare it; see moq::shutdown.
extern "C" void moq_ffi_shutdown(void);

// uniffi::expected is std::expected or tl::expected depending on the standard, and the
// generated sources (src/moq.cpp) and every includer must agree. The bindings define the
// symbol for the type they saw and every includer references the one it sees, so a mismatch
// fails to link instead of corrupting memory.
#if defined(__cpp_lib_expected) && __cpp_lib_expected >= 202211L
#define MOQ_ABI moq_abi_std_expected
#define MOQ_ABI_NAME "moq_abi_std_expected"
#else
#define MOQ_ABI moq_abi_tl_expected
#define MOQ_ABI_NAME "moq_abi_tl_expected"
#endif
extern "C" const int MOQ_ABI;
#if defined(MOQ_IMPLEMENTATION)
extern "C" const int MOQ_ABI = 1;
#elif defined(_MSC_VER)
#pragma comment(linker, "/include:" MOQ_ABI_NAME)
#else
namespace moq::detail {
[[maybe_unused]] __attribute__((used)) static const int *const abi = &MOQ_ABI;
} // namespace moq::detail
#endif

namespace moq {

// Every generated `moq::MoqFoo` is also `moq::Foo`. `just cpp check` fails when one is missing.
using AnnounceConsumer = MoqAnnounceConsumer;
using AnnounceUpdate = MoqAnnounceUpdate;
using AnnouncedBroadcast = MoqAnnouncedBroadcast;
using AudioCodec = MoqAudioCodec;
using AudioConsumer = MoqAudioConsumer;
using AudioProducer = MoqAudioProducer;
using Bandwidth = MoqBandwidth;
using BroadcastConsumer = MoqBroadcastConsumer;
using BroadcastDynamic = MoqBroadcastDynamic;
using BroadcastProducer = MoqBroadcastProducer;
using BroadcastRequest = MoqBroadcastRequest;
using CatalogConsumer = MoqCatalogConsumer;
using Client = MoqClient;
using ContainerProducer = MoqContainerProducer;
using ContainerStreamProducer = MoqContainerStreamProducer;
using GroupConsumer = MoqGroupConsumer;
using GroupProducer = MoqGroupProducer;
using GroupRequest = MoqGroupRequest;
using JsonSnapshotConsumer = MoqJsonSnapshotConsumer;
using JsonSnapshotProducer = MoqJsonSnapshotProducer;
using JsonStreamConsumer = MoqJsonStreamConsumer;
using JsonStreamProducer = MoqJsonStreamProducer;
using MediaConsumer = MoqMediaConsumer;
using MediaGroupConsumer = MoqMediaGroupConsumer;
using MediaProducer = MoqMediaProducer;
using MediaStreamProducer = MoqMediaStreamProducer;
using OriginConsumer = MoqOriginConsumer;
using OriginDynamic = MoqOriginDynamic;
using OriginProducer = MoqOriginProducer;
using Request = MoqRequest;
using Reservation = MoqReservation;
using Server = MoqServer;
using Session = MoqSession;
using TrackConsumer = MoqTrackConsumer;
using TrackDemand = MoqTrackDemand;
using TrackDynamic = MoqTrackDynamic;
using TrackProducer = MoqTrackProducer;
using TrackRequest = MoqTrackRequest;
using VideoConsumer = MoqVideoConsumer;
using VideoProducer = MoqVideoProducer;
using AnnounceConfig = MoqAnnounceConfig;
using Audio = MoqAudio;
using AudioDecoderOutput = MoqAudioDecoderOutput;
using AudioEncoderInput = MoqAudioEncoderInput;
using AudioEncoderOutput = MoqAudioEncoderOutput;
using AudioFrame = MoqAudioFrame;
using AudioInit = MoqAudioInit;
using Backoff = MoqBackoff;
using Catalog = MoqCatalog;
using ConnectionStats = MoqConnectionStats;
using ContainerInit = MoqContainerInit;
using Datagram = MoqDatagram;
using Dimensions = MoqDimensions;
using FetchGroupOptions = MoqFetchGroupOptions;
using Frame = MoqFrame;
using JsonSnapshotConfig = MoqJsonSnapshotConfig;
using JsonStreamConfig = MoqJsonStreamConfig;
using MediaFrame = MoqMediaFrame;
using OriginConfig = MoqOriginConfig;
using ProtocolError = MoqProtocolError;
using Route = MoqRoute;
using Subscription = MoqSubscription;
using TrackInfo = MoqTrackInfo;
using Video = MoqVideo;
using VideoDecodedFrame = MoqVideoDecodedFrame;
using VideoDecoderOutput = MoqVideoDecoderOutput;
using VideoEncoderInput = MoqVideoEncoderInput;
using VideoEncoderOutput = MoqVideoEncoderOutput;
using VideoFrame = MoqVideoFrame;
using VideoHint = MoqVideoHint;
using VideoInit = MoqVideoInit;
using VideoProperties = MoqVideoProperties;
using AudioFormat = MoqAudioFormat;
using AudioSampleFormat = MoqAudioSampleFormat;
using ConnectionStatus = MoqConnectionStatus;
using Container = MoqContainer;
using ContainerFormat = MoqContainerFormat;
using Error = MoqError;
using ErrorScope = MoqErrorScope;
using ProtocolKind = MoqProtocolKind;
using Transport = MoqTransport;
using VideoCodec = MoqVideoCodec;
using VideoEncoderKind = MoqVideoEncoderKind;
using VideoFormat = MoqVideoFormat;
using VideoPixelFormat = MoqVideoPixelFormat;

// The value of a fallible call, or the Error that stopped it. std::expected on C++23.
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

// Stops the moq-ffi runtime thread, then the executor, before the process or module goes away.
// Pending calls resolve Cancelled. Call it once from a thread that is not running a continuation.
inline void shutdown() noexcept {
    moq_ffi_shutdown();
    ::uniffi::shutdown_async_dispatcher();
}

// Sets the log level: "error", "warn", "info", "debug", "trace", or "". Errors if called twice.
inline expected<void> log_level(const std::string &level) {
    return moq_log_level(level);
}

#ifdef MOQ_COROUTINES
namespace detail {

// Resumes a coroutine with a future's result. The state is shared with the continuation, so
// whichever of completion and suspension comes second does the resuming.
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
            std::lock_guard<std::mutex> guard(state_->mutex);
            state_->handle = nullptr;
            continuation = std::move(state_->continuation);
        }
    }

    bool await_ready() const noexcept {
        return false;
    }

    bool await_suspend(std::coroutine_handle<> handle) noexcept {
        auto continuation = std::move(future_).then(inline_executor, [state = state_](Output output) {
            std::coroutine_handle<> resume;
            {
                std::lock_guard<std::mutex> guard(state->mutex);
                state->output.emplace(std::move(output));
                resume = std::exchange(state->handle, nullptr);
            }
            if (resume) {
                resume.resume();
            }
        });

        std::lock_guard<std::mutex> guard(state_->mutex);
        state_->continuation.emplace(std::move(continuation));
        if (state_->output) {
            // Completed before suspending: carry on without a round trip.
            return false;
        }
        state_->handle = handle;
        return true;
    }

    Output await_resume() noexcept {
        std::lock_guard<std::mutex> guard(state_->mutex);
        return std::move(*state_->output);
    }

private:
    struct State {
        std::mutex mutex;
        std::coroutine_handle<> handle;
        std::optional<Output> output;
        std::optional<Continuation> continuation;
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
