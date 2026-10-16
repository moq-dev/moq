import MoqFFI

/// Read side of a video track decoded inside the bindings. Iterating yields
/// frames that own their decoded picture; call `pixels(format:)` for tightly
/// packed CPU pixels. Release frames promptly: held frames hold decoder buffers.
public final class VideoDecoder: AsyncSequence, Sendable {
    /// The decoded video frame emitted by this sequence.
    public typealias Element = VideoDecodedFrame

    let ffi: MoqVideoDecoder

    init(_ ffi: MoqVideoDecoder) {
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

    /// A watch-only handle to whether the video track has subscribers.
    public func demand() throws -> TrackDemand {
        TrackDemand(try ffi.demand())
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
    /// something the encoder can't see such as a scene change. Throws if the
    /// selected encoder cannot force a keyframe; nothing is queued then and
    /// groups keep their interval.
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

    /// This encoder's bandwidth reservation, if published against a session allocator.
    public func reservation() -> Reservation? {
        ffi.reservation().map(Reservation.init)
    }

    /// Flush any frames the codec is holding and finalize the track.
    public func finish() throws {
        try ffi.finish()
    }
}

/// Native video encoding, publication, and decoding.
public enum Video {
    public typealias Producer = VideoProducer
    public typealias EncoderInput = VideoEncoderInput
    public typealias EncoderOutput = VideoEncoderOutput
    public typealias DecoderOutput = VideoDecoderOutput
    public typealias Frame = VideoFrame
    public typealias Decoder = VideoDecoder

    public static func producer(
        broadcast: BroadcastProducer,
        catalog: Media.CatalogProducer,
        input: EncoderInput,
        output: EncoderOutput,
        bandwidth: Bandwidth? = nil
    ) throws -> Producer {
        VideoProducer(try MoqVideoProducer(
            broadcast: broadcast.ffi,
            catalog: catalog.ffi,
            input: input,
            output: output,
            bandwidth: bandwidth?.ffi
        ))
    }

    public static func decoder(
        broadcast: BroadcastConsumer,
        name: String,
        rendition: Media.Video,
        output: DecoderOutput = DecoderOutput()
    ) async throws -> Decoder {
        VideoDecoder(try await moqVideoDecode(
            broadcast: broadcast.ffi,
            name: name,
            catalogVideo: rendition,
            output: output
        ))
    }

    public final class Encoder: Sendable {
        let ffi: MoqVideoEncoder

        public init(input: EncoderInput, output: EncoderOutput) throws {
            ffi = try MoqVideoEncoder(input: input, output: output)
        }
        public func encode(_ frame: Frame) throws -> [MoqVideoPacket] { try ffi.encode(frame: frame) }
        public func cut() throws { try ffi.cut() }
        public func setBitrate(_ bitrate: UInt64) throws { try ffi.setBitrate(bitrate: bitrate) }
        public func flush() throws -> [MoqVideoPacket] { try ffi.flush() }
        public func finish() throws -> [MoqVideoPacket] { try ffi.finish() }
    }
}
