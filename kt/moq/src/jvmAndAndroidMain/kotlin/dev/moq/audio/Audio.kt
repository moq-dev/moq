package dev.moq.audio

/** Encodes PCM and publishes it as a track; construct it from the broadcast's `CatalogProducer`. */
typealias Producer = uniffi.moq.MoqAudioProducer
/** Encodes PCM without publishing it. */
typealias Encoder = uniffi.moq.MoqAudioEncoder
/** Subscribes to a catalog audio rendition and yields each [Read]; open one with `Consumer.subscribe`. */
typealias Consumer = uniffi.moq.MoqAudioConsumer
/** The PCM layout written to a [Producer]. */
typealias EncoderInput = uniffi.moq.MoqAudioEncoderInput
/** The codec side: codec, rate, channels, bitrate, and frame duration (0 takes the codec's own frame). */
typealias EncoderOutput = uniffi.moq.MoqAudioEncoderOutput
/** The PCM layout a [Consumer] delivers, how stale a group may get, and its playout delay. */
typealias DecoderOutput = uniffi.moq.MoqAudioDecoderOutput
/** PCM samples in the configured layout with their presentation timestamp. */
typealias Frame = uniffi.moq.MoqAudioFrame
/** One item a [Consumer] reads, in delivery order: [ReadFrame], [ReadGap], or [ReadEnd]. */
typealias Read = uniffi.moq.MoqAudioRead
/** Decoded PCM. */
typealias ReadFrame = uniffi.moq.MoqAudioRead.Frame
/** Media after what was already read was lost; what follows does not continue it. */
typealias ReadGap = uniffi.moq.MoqAudioRead.Gap
/** The publisher stopped its encoder; frames starting before `endUs` are its remaining media. */
typealias ReadEnd = uniffi.moq.MoqAudioRead.End

/** One encoded payload from an [Encoder]. */
typealias Packet = uniffi.moq.MoqAudioPacket
/** The packets an [Encoder] drains on finish and the padded samples to discard. */
typealias EncoderFinish = uniffi.moq.MoqAudioEncoderFinish
/** The audio codec: `Codec.opus()` or `Codec.aac()`. */
typealias Codec = uniffi.moq.MoqAudioCodec
/** A raw PCM sample layout. */
typealias SampleFormat = uniffi.moq.MoqAudioSampleFormat

/** Input, codec settings, and publication ownership for a producer. */
typealias ProducerOptions = uniffi.moq.MoqAudioProducerOptions
