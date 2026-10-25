import MoqFFI

/// Native video encoding, publication, and decoding.
public enum Video {
    /// An exact video frame rate in frames per second.
    public typealias FrameRate = MoqFFI.MoqVideoFrameRate

    /// The raw frame layout, resolution, and framerate the caller writes.
    public typealias EncoderInput = VideoEncoderInput
    /// The codec, bitrate, group mode, and backend preference.
    public typealias EncoderOutput = VideoEncoderOutput
    /// What a `Consumer` delivers: an optional resize, a max delay, and the decoder's surface (macOS only).
    public typealias DecoderOutput = VideoDecoderOutput
    /// One raw picture with its presentation timestamp.
    public typealias Frame = VideoFrame
    /// One encoded access unit from an `Encoder`.
    public typealias Packet = MoqVideoPacket
    /// How pictures are split into independently decodable groups.
    public typealias Gop = MoqVideoGop
    /// A video codec identifier (H.264 or H.265).
    public typealias Codec = VideoCodec
    /// A CPU pixel layout (I420 or RGBA).
    public typealias PixelFormat = VideoPixelFormat
    /// Which encoder implementation to use: automatic, hardware, software, or one named backend.
    public typealias EncoderKind = VideoEncoderKind

    /// Write side of a raw-video track. Pixels written here are encoded (H.264 or
    /// H.265) inside the FFI boundary per the `EncoderInput`/`EncoderOutput` given
    /// at construction.
    public final class Producer: Sendable {
        let ffi: MoqVideoProducer

        /// Open a video track on the catalog's broadcast and register its rendition.
        public init(
            catalog: Media.CatalogProducer,
            input: EncoderInput,
            output: EncoderOutput,
            track: String? = nil,
            bandwidth: Allocator? = nil
        ) throws {
            ffi = try MoqVideoProducer(
                catalog: catalog.ffi,
                options: MoqVideoProducerOptions(
                    input: input, output: output, track: track, bandwidth: bandwidth?.ffi))
        }

        /// A watch-only handle to whether the video track has subscribers.
        public func demand() throws -> TrackDemand {
            TrackDemand(try ffi.demand())
        }

        /// Encode and publish one raw frame.
        ///
        /// A hardware encoder pipelines, so a call that puts nothing on the wire is
        /// normal rather than an error.
        public func write(_ frame: Frame) throws {
            try ffi.write(frame: frame)
        }

        /// Start a new group at the next written frame.
        ///
        /// Optional: the encoder starts groups on its own per `output.gop`, so a
        /// subscriber can always join without this. Reach for it only to place the
        /// boundaries yourself, aligning groups with something the encoder can't
        /// see such as a scene change. Throws if the selected encoder cannot force
        /// a keyframe; nothing is queued then and groups keep their interval.
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

    /// Read side of a video track decoded inside the bindings. Iterating yields
    /// frames that own their decoded picture; call `pixels(format:)` for tightly
    /// packed CPU pixels. Release frames promptly: held frames hold decoder buffers.
    public final class Consumer: AsyncSequence, Sendable {
        /// The decoded video frame emitted by this sequence.
        public typealias Element = VideoDecodedFrame

        let ffi: MoqVideoConsumer

        init(_ ffi: MoqVideoConsumer) {
            self.ffi = ffi
        }

        /// Subscribe to the catalog video rendition `id` and decode it.
        public static func subscribe(
            broadcast: BroadcastConsumer,
            id: String,
            rendition: Media.Video,
            output: DecoderOutput = DecoderOutput()
        ) async throws -> Consumer {
            Consumer(try await MoqVideoConsumer.subscribe(
                broadcast: broadcast.ffi,
                id: id,
                catalogVideo: rendition,
                output: output
            ))
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

    /// Encodes raw pictures without publishing them.
    public final class Encoder: Sendable {
        let ffi: MoqVideoEncoder

        /// Open a codec-only encoder.
        public init(input: EncoderInput, output: EncoderOutput) throws {
            ffi = try MoqVideoEncoder(input: input, output: output)
        }

        /// Encode one picture, returning any access units now available.
        public func encode(_ frame: Frame) throws -> [Packet] { try ffi.encode(frame: frame) }

        /// Force the next encoded picture to start a new group.
        public func cut() throws { try ffi.cut() }

        /// Retune the live encoder in bits per second.
        public func setBitrate(_ bitrate: UInt64) throws { try ffi.setBitrate(bitrate: bitrate) }

        /// Drain buffered access units and keep the encoder open.
        public func flush() throws -> [Packet] { try ffi.flush() }

        /// Drain and close the encoder.
        public func finish() throws -> [Packet] { try ffi.finish() }
    }
}
