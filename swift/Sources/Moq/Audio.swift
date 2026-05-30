import MoqFFI

/// Read side of a raw-audio track. Iterating yields decoded PCM frames in the
/// layout declared by the `AudioDecoderOutput` passed at subscribe time.
public final class AudioConsumer: AsyncSequence, Sendable {
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

    /// Encode and write one PCM frame.
    public func write(_ frame: AudioFrame) throws {
        try ffi.write(frame: frame)
    }

    /// Finish the track and finalize encoding.
    public func finish() throws {
        try ffi.finish()
    }
}
