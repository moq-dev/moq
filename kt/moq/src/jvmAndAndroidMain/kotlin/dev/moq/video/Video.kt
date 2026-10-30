package dev.moq.video

/**
 * Encodes raw pictures and publishes them as a track; construct it from the broadcast's
 * `CatalogProducer`.
 */
typealias Producer = uniffi.moq.MoqVideoProducer

/** Encodes raw pictures without publishing them. */
typealias Encoder = uniffi.moq.MoqVideoEncoder

/**
 * Subscribes to a catalog video rendition and yields each [Read]; open one with
 * `Consumer.subscribe`.
 */
typealias Consumer = uniffi.moq.MoqVideoConsumer

/** The pixel layout, resolution, and framerate written to a [Producer]. */
typealias EncoderInput = uniffi.moq.MoqVideoEncoderInput

/** The codec, bitrate, group mode, and backend preference. */
typealias EncoderOutput = uniffi.moq.MoqVideoEncoderOutput

/**
 * What a [Consumer] delivers: an optional resize, a max delay, and the decoder's surface (macOS
 * only).
 */
typealias DecoderOutput = uniffi.moq.MoqVideoDecoderOutput

/** One raw picture with its presentation timestamp. */
typealias Frame = uniffi.moq.MoqVideoFrame

/** One item a [Consumer] reads, in delivery order: [ReadFrame], [ReadGap], or [ReadEnd]. */
typealias Read = uniffi.moq.MoqVideoRead

/** A decoded picture; `close()` its frame promptly, since held frames stall the decoder. */
typealias ReadFrame = uniffi.moq.MoqVideoRead.Frame

/** Media after what was already read was lost; what follows does not continue it. */
typealias ReadGap = uniffi.moq.MoqVideoRead.Gap

/** The publisher stopped its encoder; what follows comes from a fresh encoder. */
typealias ReadEnd = uniffi.moq.MoqVideoRead.End

/** One encoded access unit from an [Encoder]. */
typealias Packet = uniffi.moq.MoqVideoPacket

/** How pictures are split into independently decodable groups. */
typealias Gop = uniffi.moq.MoqVideoGop

/** H.264 or H.265. */
typealias Codec = uniffi.moq.MoqVideoCodec

/** A CPU pixel layout (I420 or RGBA). */
typealias PixelFormat = uniffi.moq.MoqVideoPixelFormat

/** The encoder backend preference. */
typealias EncoderKind = uniffi.moq.MoqVideoEncoderKind

/** Input, codec settings, and publication ownership for a producer. */
typealias ProducerOptions = uniffi.moq.MoqVideoProducerOptions

/** An exact video frame rate in frames per second. */
typealias FrameRate = uniffi.moq.MoqVideoFrameRate
