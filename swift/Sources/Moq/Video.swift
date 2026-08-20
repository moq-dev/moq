import MoqFFI

/// Read side of a video track decoded inside the bindings. Iterating yields
/// tightly-packed I420 frames, each carrying the size it actually decoded to.
public final class VideoConsumer: AsyncSequence, Sendable {
    /// The decoded video frame emitted by this sequence.
    public typealias Element = VideoDecodedFrame

    let ffi: MoqVideoConsumer

    init(_ ffi: MoqVideoConsumer) {
        self.ffi = ffi
    }

    /// The next frame, or `nil` once the track ends or is closed.
    public func next() async throws -> VideoDecodedFrame? {
        try await ffi.next()
    }

    /// Cancel all current and future reads.
    public func cancel() {
        ffi.cancel()
    }

    /// Create an iterator that cancels native reads when iteration ends.
    public func makeAsyncIterator() -> AsyncThrowingStream<VideoDecodedFrame, Swift.Error>.Iterator {
        moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
            try await ffi.next()
        }.makeAsyncIterator()
    }
}

/// Write side of a raw-video track. Pixels written here are encoded (H.264 or
/// H.265) inside the FFI boundary per the `VideoEncoderInput`/`Output` from
/// publish time.
public final class VideoProducer: Sendable {
    let ffi: MoqVideoProducer

    init(_ ffi: MoqVideoProducer) {
        self.ffi = ffi
    }

    /// Encode and publish one raw frame.
    ///
    /// A hardware encoder pipelines, so a call that puts nothing on the wire is
    /// normal rather than an error.
    public func write(_ frame: VideoFrame) throws {
        try ffi.write(frame: frame)
    }

    /// Start a new group at the next written frame.
    ///
    /// Optional: the encoder keyframes every `gop` frames on its own, and each
    /// of those cuts a group, so a subscriber can always join without this.
    /// Reach for it only to place the boundaries yourself, aligning groups with
    /// something the encoder can't see such as a scene change.
    public func cut() throws {
        try ffi.cut()
    }

    /// Retune the live encoder, in bits per second.
    ///
    /// Cheap enough to drive from a congestion controller: no keyframe is
    /// forced. Throws if this backend cannot retune while running. That is not
    /// fatal: the encoder keeps its current rate.
    public func setBitrate(_ bitrate: UInt64) throws {
        try ffi.setBitrate(bitrate: bitrate)
    }

    /// Flush any frames the codec is holding and finalize the track.
    public func finish() throws {
        try ffi.finish()
    }
}
