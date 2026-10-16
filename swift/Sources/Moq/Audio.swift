import MoqFFI

/// Read side of a raw-audio track. Iterating yields decoded PCM frames in the
/// layout declared by the `AudioDecoderOutput` passed at subscribe time.
public final class AudioDecoder: AsyncSequence, Sendable {
    /// The decoded PCM audio frame emitted by this sequence.
    public typealias Element = AudioFrame

    let ffi: MoqAudioDecoder

    init(_ ffi: MoqAudioDecoder) {
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

    /// A watch-only handle to whether the audio track has subscribers.
    public func demand() throws -> TrackDemand {
        TrackDemand(try ffi.demand())
    }

    /// Re-anchor the timeline to the next frame after an idle gap.
    public func reanchor() throws {
        try ffi.reanchor()
    }

    /// Encode and write one PCM frame.
    public func write(_ frame: AudioFrame) throws {
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

/// Native audio encoding, publication, and decoding.
public enum Audio {
    public typealias Producer = AudioProducer
    public typealias EncoderInput = AudioEncoderInput
    public typealias EncoderOutput = AudioEncoderOutput
    public typealias DecoderOutput = AudioDecoderOutput
    public typealias Frame = AudioFrame
    public typealias Decoder = AudioDecoder

    /// Encode PCM and publish it into a broadcast's catalog.
    public static func producer(
        broadcast: BroadcastProducer,
        catalog: Media.CatalogProducer,
        input: EncoderInput,
        output: EncoderOutput,
        bandwidth: Bandwidth? = nil
    ) throws -> Producer {
        AudioProducer(try MoqAudioProducer(
            broadcast: broadcast.ffi,
            catalog: catalog.ffi,
            input: input,
            output: output,
            bandwidth: bandwidth?.ffi
        ))
    }

    /// Subscribe to and decode one catalog audio rendition.
    public static func decoder(
        broadcast: BroadcastConsumer,
        name: String,
        rendition: Media.Audio,
        output: DecoderOutput
    ) async throws -> Decoder {
        AudioDecoder(try await moqAudioDecode(
            broadcast: broadcast.ffi,
            name: name,
            catalogAudio: rendition,
            output: output
        ))
    }

    /// A codec-only audio encoder.
    public final class Encoder: Sendable {
        let ffi: MoqAudioEncoder

        public init(output: EncoderOutput) throws { ffi = try MoqAudioEncoder(output: output) }
        public var frameSize: UInt64 {
            get throws { try ffi.frameSize() }
        }
        public func encode(_ pcm: [Float]) throws -> MoqAudioPacket { try ffi.encode(pcm: pcm) }
        public func finish(_ pcm: [Float] = []) throws -> MoqAudioEncoderFinish { try ffi.finish(pcm: pcm) }
    }
}
