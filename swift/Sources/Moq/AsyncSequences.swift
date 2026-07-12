import Foundation
@_exported import MoqFFI

extension MoqCatalogConsumer {
    /// Stream of catalog updates. Terminates when the underlying track ends.
    public var updates: AsyncThrowingStream<MoqCatalog, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    while let next = try await self.next() {
                        try Task.checkCancellation()
                        continuation.yield(next)
                    }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { [weak self] _ in
                task.cancel()
                self?.cancel()
            }
        }
    }
}

extension MoqMediaConsumer {
    /// Stream of decoded media frames in decode order. Terminates when the underlying track ends.
    public var frames: AsyncThrowingStream<MoqFrame, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    while let frame = try await self.next() {
                        try Task.checkCancellation()
                        continuation.yield(frame)
                    }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { [weak self] _ in
                task.cancel()
                self?.cancel()
            }
        }
    }
}

extension MoqAudioConsumer {
    /// Stream of decoded audio frames in the layout declared by
    /// `MoqAudioDecoderConfig`. Terminates when the underlying track ends.
    public var frames: AsyncThrowingStream<MoqAudioFrame, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    while let frame = try await self.next() {
                        try Task.checkCancellation()
                        continuation.yield(frame)
                    }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { [weak self] _ in
                task.cancel()
                self?.cancel()
            }
        }
    }
}

extension MoqJsonConsumer {
    /// Stream of JSON values (as strings), yielding the latest reconstructed value. A consumer
    /// that has fallen behind collapses the backlog. Terminates when the underlying track ends.
    public var values: AsyncThrowingStream<String, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    while let value = try await self.next() {
                        try Task.checkCancellation()
                        continuation.yield(value)
                    }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { [weak self] _ in
                task.cancel()
                self?.cancel()
            }
        }
    }
}

extension MoqJsonStreamConsumer {
    /// Stream of JSON records (as strings) in order. Terminates when the underlying track ends.
    public var values: AsyncThrowingStream<String, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    while let value = try await self.next() {
                        try Task.checkCancellation()
                        continuation.yield(value)
                    }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { [weak self] _ in
                task.cancel()
                self?.cancel()
            }
        }
    }
}

extension MoqTrackConsumer {
    /// Stream of groups in sequence order, skipping forward if the reader falls behind.
    public var groups: AsyncThrowingStream<MoqGroupConsumer, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    while let group = try await self.nextGroup() {
                        try Task.checkCancellation()
                        continuation.yield(group)
                    }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { [weak self] _ in
                task.cancel()
                self?.cancel()
            }
        }
    }

    /// Stream of groups in arrival order, including out-of-sequence deliveries.
    public var groupsAsArrived: AsyncThrowingStream<MoqGroupConsumer, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    while let group = try await self.recvGroup() {
                        try Task.checkCancellation()
                        continuation.yield(group)
                    }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { [weak self] _ in
                task.cancel()
                self?.cancel()
            }
        }
    }
}

extension MoqBroadcastDynamic {
    /// Stream of tracks requested by subscribers.
    public var requestedTracks: AsyncThrowingStream<MoqTrackProducer, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    while true {
                        let track = try await self.requestedTrack()
                        try Task.checkCancellation()
                        continuation.yield(track)
                    }
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { [weak self] _ in
                task.cancel()
                self?.cancel()
            }
        }
    }
}

extension MoqGroupConsumer {
    /// Stream of raw frame payloads in this group.
    public var frames: AsyncThrowingStream<Data, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    while let frame = try await self.readFrame() {
                        try Task.checkCancellation()
                        continuation.yield(frame)
                    }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { [weak self] _ in
                task.cancel()
                self?.cancel()
            }
        }
    }
}

extension MoqAnnounced {
    /// Stream of broadcast announcements. Terminates when the origin closes.
    public var announcements: AsyncThrowingStream<MoqAnnouncement, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    while let announcement = try await self.next() {
                        try Task.checkCancellation()
                        continuation.yield(announcement)
                    }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { [weak self] _ in
                task.cancel()
                self?.cancel()
            }
        }
    }
}
