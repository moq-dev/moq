import Foundation
import MoqFFI

/// Bridge a `next()`-style native consumer into an `AsyncThrowingStream`.
///
/// Pull-per-demand: `next` is called only when the consumer asks for the next
/// element, never ahead of it. This is what preserves the FFI jitter buffer's
/// fall-behind skipping (`latencyMaxMs` GoP-skipping): draining eagerly would
/// move the backlog into an unbounded Swift buffer, so a slow consumer would
/// grow memory and receive stale groups instead of skipping forward.
///
/// Pulls until `next` returns nil (the track/origin ended) or throws. Cancelling
/// the consuming task calls `cancel` to release the native handle and unblock any
/// in-flight read.
func moqStream<Element>(
    cancel: @escaping @Sendable () -> Void,
    next: @escaping @Sendable () async throws -> Element?
) -> AsyncThrowingStream<Element, Swift.Error> {
    AsyncThrowingStream(unfolding: {
        try await withTaskCancellationHandler {
            try await next()
        } onCancel: {
            cancel()
        }
    })
}
