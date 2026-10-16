package dev.moq.video

import dev.moq.Bandwidth
import dev.moq.BroadcastConsumer
import dev.moq.BroadcastProducer
import dev.moq.media.CatalogProducer
import dev.moq.media.Video

typealias Producer = uniffi.moq.MoqVideoProducer
typealias Encoder = uniffi.moq.MoqVideoEncoder
typealias Decoder = uniffi.moq.MoqVideoDecoder
typealias EncoderInput = uniffi.moq.MoqVideoEncoderInput
typealias EncoderOutput = uniffi.moq.MoqVideoEncoderOutput
typealias DecoderOutput = uniffi.moq.MoqVideoDecoderOutput
typealias Frame = uniffi.moq.MoqVideoFrame
typealias Packet = uniffi.moq.MoqVideoPacket
typealias Gop = uniffi.moq.MoqVideoGop
typealias Codec = uniffi.moq.MoqVideoCodec
typealias PixelFormat = uniffi.moq.MoqVideoPixelFormat
typealias EncoderKind = uniffi.moq.MoqVideoEncoderKind

fun producer(broadcast: BroadcastProducer, catalog: CatalogProducer, input: EncoderInput, output: EncoderOutput, bandwidth: Bandwidth? = null): Producer =
    Producer(broadcast, catalog, input, output, bandwidth)

suspend fun decoder(broadcast: BroadcastConsumer, name: String, rendition: Video, output: DecoderOutput = DecoderOutput()): Decoder =
    uniffi.moq.moqVideoDecode(broadcast, name, rendition, output)
