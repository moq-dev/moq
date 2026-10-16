package dev.moq.audio

import dev.moq.Bandwidth
import dev.moq.BroadcastConsumer
import dev.moq.BroadcastProducer
import dev.moq.media.Audio
import dev.moq.media.CatalogProducer

typealias Producer = uniffi.moq.MoqAudioProducer
typealias Encoder = uniffi.moq.MoqAudioEncoder
typealias Decoder = uniffi.moq.MoqAudioDecoder
typealias EncoderInput = uniffi.moq.MoqAudioEncoderInput
typealias EncoderOutput = uniffi.moq.MoqAudioEncoderOutput
typealias DecoderOutput = uniffi.moq.MoqAudioDecoderOutput
typealias Frame = uniffi.moq.MoqAudioFrame
typealias Packet = uniffi.moq.MoqAudioPacket
typealias EncoderFinish = uniffi.moq.MoqAudioEncoderFinish
typealias Codec = uniffi.moq.MoqAudioCodec
typealias SampleFormat = uniffi.moq.MoqAudioSampleFormat

fun producer(broadcast: BroadcastProducer, catalog: CatalogProducer, input: EncoderInput, output: EncoderOutput, bandwidth: Bandwidth? = null): Producer =
    Producer(broadcast, catalog, input, output, bandwidth)

suspend fun decoder(broadcast: BroadcastConsumer, name: String, rendition: Audio, output: DecoderOutput): Decoder =
    uniffi.moq.moqAudioDecode(broadcast, name, rendition, output)
