import Foundation
import MoqFFI

/// Bridge a `next()`-style native consumer into an `AsyncThrowingStream`.
///
/// Pulls from `next` until it returns nil (the track/origin ended), finishes on
/// error, and calls `cancel` when the consuming task terminates, so a broken
/// `for await` loop or a cancelled parent `Task` releases the native handle and
/// unblocks any in-flight read.
func moqStream<Element>(
    cancel: @escaping @Sendable () -> Void,
    next: @escaping @Sendable () async throws -> Element?
) -> AsyncThrowingStream<Element, Swift.Error> {
    AsyncThrowingStream { continuation in
        let task = Task {
            do {
                while let item = try await next() {
                    try Task.checkCancellation()
                    continuation.yield(item)
                }
                continuation.finish()
            } catch {
                continuation.finish(throwing: error)
            }
        }
        continuation.onTermination = { _ in
            task.cancel()
            cancel()
        }
    }
}
