import MoqFFI

/// Read side of a raw-audio track. Iterating yields decoded PCM frames in the
/// layout declared by the `AudioDecoderOutput` passed at subscribe time.
public final class AudioConsumer: AsyncSequence, Sendable {
    /// The decoded PCM audio frame emitted by this sequence.
    public typealias Element = AudioFrame

    let ffi: MoqAudioConsumer

    init(_ ffi: MoqAudioConsumer) {
        self.ffi = ffi
    }

    /// The next frame, or `nil` once the track ends or is closed.
    public func next() async throws -> AudioFrame? {
        try await ffi.next()
    }

    /// Cancel all current and future reads.
    public func cancel() {
        ffi.cancel()
    }

    /// Create an iterator that cancels native reads when iteration ends.
    public func makeAsyncIterator() -> AsyncThrowingStream<AudioFrame, Swift.Error>.Iterator {
        moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
            try await ffi.next()
        }.makeAsyncIterator()
    }
}

/// Write side of a raw-audio track. PCM written here is encoded (e.g. to Opus)
/// inside the FFI boundary per the `AudioEncoderInput`/`Output` from publish time.
public final class AudioProducer: Sendable {
    let ffi: MoqAudioProducer

    init(_ ffi: MoqAudioProducer) {
        self.ffi = ffi
    }

    /// The audio track's name.
    public var name: String {
        get throws { try ffi.name() }
    }

    /// Suspend until the audio track has at least one active consumer.
    public func used() async throws {
        try await ffi.used()
    }

    /// Suspend until the audio track has no active consumers.
    public func unused() async throws {
        try await ffi.unused()
    }

    /// Re-anchor the timeline to the next frame after an idle gap.
    public func resetEpoch() throws {
        try ffi.resetEpoch()
    }

    /// Encode and write one PCM frame.
    public func write(_ frame: AudioFrame) throws {
        try ffi.write(frame: frame)
    }

    /// Finish the track and finalize encoding.
    public func finish() throws {
        try ffi.finish()
    }
}
