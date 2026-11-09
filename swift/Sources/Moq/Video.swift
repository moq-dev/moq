import Foundation
import MoqFFI

/// Native video encoding, publication, and decoding.
public enum Video {
    /// An exact video frame rate in frames per second.
    public typealias FrameRate = MoqFFI.MoqVideoFrameRate

    /// The raw frame layout, resolution, and framerate the caller writes.
    public typealias EncoderInput = VideoEncoderInput
    /// The codec, bitrate, join interval, and backend preference.
    public typealias EncoderOutput = VideoEncoderOutput
    /// What a `Consumer` delivers: an optional resize, a delay, and the decoder's surface (macOS only).
    public typealias DecoderOutput = VideoDecoderOutput
    /// One raw picture with its presentation timestamp.
    public typealias Frame = VideoFrame
    /// One item a `Consumer` reads, in delivery order: a decoded `.frame`, a `.gap`
    /// where media was lost, or an `.end` where the publisher stopped its encoder.
    public enum Read: Sendable {
        /// A decoded picture; release it promptly, since held frames stall the decoder.
        case frame(DecodedFrame)
        /// Media after `timestamp` was lost; what follows resumes within `duration` and does not continue it.
        case gap(timestamp: Timestamp, duration: TimeInterval)
        /// The publisher stopped its encoder at `end`; what follows comes from a fresh encoder.
        case end(Timestamp)

        init(_ read: MoqVideoRead) {
            switch read {
            case .frame(let frame): self = .frame(DecodedFrame(frame))
            case .gap(let timestamp, let duration): self = .gap(timestamp: timestamp, duration: duration)
            case .end(let end): self = .end(end)
            }
        }
    }

    /// One decoded picture, owning the decoder's surface until released;
    /// `pixels(format:)` converts it to packed CPU pixels.
    public final class DecodedFrame: Sendable {
        let ffi: MoqVideoDecodedFrame

        init(_ ffi: MoqVideoDecodedFrame) {
            self.ffi = ffi
        }

        /// The presentation timestamp.
        public func timestamp() -> Timestamp { ffi.timestamp() }

        /// The decoded width in pixels; `resize` is only a hint.
        public func width() -> UInt32 { ffi.width() }

        /// The decoded height in pixels.
        public func height() -> UInt32 { ffi.height() }

        /// The picture converted to tightly packed CPU pixels in `format`.
        public func pixels(format: PixelFormat) throws -> Data { try ffi.pixels(format: format) }

        /// The decoder's platform surface, or `nil` when the frame is in CPU memory.
        public func surface() -> Surface? { ffi.surface().map(Surface.init) }
    }

    /// The platform image backing a `DecodedFrame`, when `DecoderOutput` asked for one.
    public enum Surface: Sendable, Equatable {
        /// A borrowed `CVPixelBufferRef` address, valid while its frame lives.
        case pixelBuffer(pointer: UInt64)
        /// A surface kind this version of the package does not know yet.
        case unknown

        init(_ surface: MoqVideoSurface) {
            // `if case` rather than `switch`, so a variant this package predates maps to `.unknown`.
            if case .pixelBuffer(let pointer) = surface {
                self = .pixelBuffer(pointer: pointer)
            } else {
                self = .unknown
            }
        }
    }
    /// One encoded access unit from an `Encoder`.
    public typealias Packet = MoqVideoPacket
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
        ///
        /// Pass `lower`, the next rendition down a simulcast ladder, to disable this
        /// one while the bandwidth grant cannot fund it; it resumes once the grant
        /// recovers. Needs `bandwidth` and a bitrate above the lower rendition's.
        public init(
            catalog: Media.CatalogProducer,
            input: EncoderInput,
            output: EncoderOutput,
            track: String? = nil,
            bandwidth: Allocator? = nil,
            lower: Producer? = nil
        ) throws {
            ffi = try MoqVideoProducer(
                catalog: catalog.ffi,
                options: MoqVideoProducerOptions(
                    input: input, output: output, track: track, bandwidth: bandwidth?.ffi,
                    lower: lower?.ffi))
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
        /// Optional: the encoder starts groups on its own per `output.join`, so a
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
    /// frames that own their decoded picture, and the gaps and encoder ends between
    /// them; call `pixels(format:)` for tightly packed CPU pixels. Release frames
    /// promptly: held frames hold decoder buffers.
    public final class Consumer: AsyncSequence, Sendable {
        /// The frame, gap, or end emitted by this sequence.
        public typealias Element = Read

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
            Consumer(
                try await MoqVideoConsumer.subscribe(
                    broadcast: broadcast.ffi,
                    id: id,
                    catalogVideo: rendition,
                    output: output
                ))
        }

        /// The next frame, gap, or end, or `nil` once the track ends or is closed.
        public func next() async throws -> Read? {
            try await ffi.next().map(Read.init)
        }

        /// Cancel all current and future reads.
        public func cancel() {
            ffi.cancel()
        }

        /// Create an iterator that cancels native reads when iteration ends.
        public func makeAsyncIterator() -> AsyncThrowingStream<Read, Swift.Error>.Iterator {

            moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
                try await ffi.next().map(Read.init)
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
