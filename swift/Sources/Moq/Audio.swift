import Foundation
import MoqFFI

/// Native audio encoding, publication, and decoding.
public enum Audio {
    /// The PCM layout (format, sample rate, channels) written to a `Producer`.
    public typealias EncoderInput = AudioEncoderInput
    /// The codec side: codec, rate, channels, bitrate, and frame duration.
    public typealias EncoderOutput = AudioEncoderOutput
    /// The PCM layout a `Consumer` delivers, how stale a group may get, and its playout delay.
    public typealias DecoderOutput = AudioDecoderOutput
    /// Interleaved PCM with its presentation timestamp.
    public typealias Frame = AudioFrame
    /// One encoded payload from an `Encoder`.
    public typealias Packet = MoqAudioPacket
    /// The packets an `Encoder` drains on finish and the padded samples to discard.
    public typealias EncoderFinish = MoqAudioEncoderFinished
    /// Selects the audio encoder codec. Build one with `Codec.opus()` or `Codec.aac()`.
    public typealias Codec = AudioCodec
    /// The PCM sample layout written to a `Producer` or delivered by a `Consumer`.
    public typealias SampleFormat = AudioSampleFormat

    /// Write side of a raw-audio track. PCM written here is encoded (e.g. to Opus)
    /// inside the FFI boundary per the `EncoderInput`/`EncoderOutput` given at construction.
    public final class Producer: Sendable {
        let ffi: MoqAudioProducer

        /// Open an audio track on the catalog's broadcast and register its rendition.
        public init(
            catalog: Media.CatalogProducer,
            input: EncoderInput,
            output: EncoderOutput,
            track: String? = nil,
            bandwidth: Bandwidth? = nil
        ) throws {
            ffi = try MoqAudioProducer(
                catalog: catalog.ffi,
                options: MoqAudioProducerOptions(
                    input: input, output: output, track: track, bandwidth: bandwidth?.ffi))
        }

        /// A watch-only handle to whether the audio track has subscribers.
        public func demand() throws -> TrackDemand {
            TrackDemand(try ffi.demand())
        }

        /// Re-anchor the timeline to the next frame after an idle gap.
        public func reanchor() throws {
            try ffi.reanchor()
        }

        /// Encode and write one PCM frame.
        public func write(_ frame: Frame) throws {
            try ffi.write(frame: frame)
        }

        /// This encoder's bandwidth reservation, if published against a session allocator.
        public func reservation() -> Reservation? {
            ffi.reservation().map(Reservation.init)
        }

        /// Finish the track and finalize encoding.
        public func finish() throws {
            try ffi.finish()
        }
    }

    /// Read side of a raw-audio track. Iterating yields decoded PCM frames in the
    /// layout declared by the `DecoderOutput` passed to `subscribe`.
    public final class Consumer: AsyncSequence, Sendable {
        /// The decoded PCM audio frame emitted by this sequence.
        public typealias Element = Frame

        let ffi: MoqAudioConsumer

        init(_ ffi: MoqAudioConsumer) {
            self.ffi = ffi
        }

        /// Subscribe to the catalog audio rendition `id` and decode it.
        public static func subscribe(
            broadcast: BroadcastConsumer,
            id: String,
            rendition: Media.Audio,
            output: DecoderOutput
        ) async throws -> Consumer {
            Consumer(try await MoqAudioConsumer.subscribe(
                broadcast: broadcast.ffi,
                id: id,
                catalogAudio: rendition,
                output: output
            ))
        }

        /// The next frame, or `nil` once the track ends or is closed.
        public func next() async throws -> Frame? {
            try await ffi.next()
        }

        /// The playout delay to hold ahead of the playhead: the fixed `DecoderOutput.delay`,
        /// or the estimate from every frame read so far.
        public var delay: TimeInterval { ffi.delay() }

        /// Cancel all current and future reads.
        public func cancel() {
            ffi.cancel()
        }

        /// Create an iterator that cancels native reads when iteration ends.
        public func makeAsyncIterator() -> AsyncThrowingStream<Frame, Swift.Error>.Iterator {
            moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
                try await ffi.next()
            }.makeAsyncIterator()
        }
    }

    /// Encodes PCM without publishing it.
    public final class Encoder: Sendable {
        let ffi: MoqAudioEncoder

        /// Open a codec-only encoder.
        public init(output: EncoderOutput) throws { ffi = try MoqAudioEncoder(output: output) }

        /// Samples per channel each `encode` call takes.
        public var frameSize: UInt64 {
            get throws { try ffi.frameSize() }
        }

        /// The OpusHead or AudioSpecificConfig that publishes these packets via `Media.TrackProducer.audio`.
        public func trackInit() throws -> Media.AudioInit { try ffi.trackInit() }

        /// Retune the live encoder in bits per second.
        public func setBitrate(_ bitrate: UInt64) throws { try ffi.setBitrate(bitrate: bitrate) }

        /// Encode one codec-sized interleaved f32 frame.
        public func encode(_ pcm: [Float]) throws -> Packet { try ffi.encode(pcm: pcm) }

        /// Pad an optional partial frame, drain the codec, and close the encoder.
        public func finish(_ pcm: [Float] = []) throws -> EncoderFinish { try ffi.finish(pcm: pcm) }
    }
}
