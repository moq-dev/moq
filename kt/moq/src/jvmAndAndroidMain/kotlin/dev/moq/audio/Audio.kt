package dev.moq.audio

/** Encodes PCM and publishes it as a track; construct it from the broadcast's `CatalogProducer`. */
typealias Producer = uniffi.moq.MoqAudioProducer
/** Encodes PCM without publishing it. */
typealias Encoder = uniffi.moq.MoqAudioEncoder
/** Subscribes to a catalog audio rendition and yields decoded PCM; open one with `Consumer.subscribe`. */
typealias Consumer = uniffi.moq.MoqAudioConsumer
/** The PCM layout written to a [Producer]. */
typealias EncoderInput = uniffi.moq.MoqAudioEncoderInput
/** The codec side: codec, rate, channels, bitrate, frame duration (0 takes the preset's or codec's own frame), and preset. */
typealias EncoderOutput = uniffi.moq.MoqAudioEncoderOutput
/** The PCM layout a [Consumer] delivers, how stale a group may get, and its playout delay. */
typealias DecoderOutput = uniffi.moq.MoqAudioDecoderOutput
/** PCM samples in the configured layout with their presentation timestamp. */
typealias Frame = uniffi.moq.MoqAudioFrame
/** One encoded payload from an [Encoder]. */
typealias Packet = uniffi.moq.MoqAudioPacket
/** The packets an [Encoder] drains on finish and the padded samples to discard. */
typealias EncoderFinish = uniffi.moq.MoqAudioEncoderFinish
/** The audio codec: `Codec.opus()` or `Codec.aac()`. */
typealias Codec = uniffi.moq.MoqAudioCodec
/** How the encoder trades latency for compression; `null` is `BALANCED`. */
typealias Preset = uniffi.moq.MoqAudioPreset
/** A raw PCM sample layout. */
typealias SampleFormat = uniffi.moq.MoqAudioSampleFormat

/** Input, codec settings, and publication ownership for a producer. */
typealias ProducerOptions = uniffi.moq.MoqAudioProducerOptions
