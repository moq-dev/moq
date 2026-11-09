package dev.moq.video

import dev.moq.BroadcastConsumer
import dev.moq.Timestamp
import uniffi.moq.MoqException
import uniffi.moq.MoqVideoRead
import uniffi.moq.MoqVideoSurface

/**
 * Encodes raw pictures and publishes them as a track; construct it from the broadcast's
 * `CatalogProducer`.
 */
typealias Producer = uniffi.moq.MoqVideoProducer

/** Encodes raw pictures without publishing them. */
typealias Encoder = uniffi.moq.MoqVideoEncoder

/** The pixel layout, resolution, and framerate written to a [Producer]. */
typealias EncoderInput = uniffi.moq.MoqVideoEncoderInput

/** The codec, bitrate, group mode, and backend preference. */
typealias EncoderOutput = uniffi.moq.MoqVideoEncoderOutput

/**
 * What a [Consumer] delivers: an optional resize, a delay, and the decoder's surface (macOS only).
 */
typealias DecoderOutput = uniffi.moq.MoqVideoDecoderOutput

/** One raw picture with its presentation timestamp. */
typealias Frame = uniffi.moq.MoqVideoFrame

/** One encoded access unit from an [Encoder]. */
typealias Packet = uniffi.moq.MoqVideoPacket

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

/**
 * Subscribes to a catalog video rendition and yields each [Read]; open one with
 * [Consumer.subscribe].
 */
class Consumer private constructor(private val inner: uniffi.moq.MoqVideoConsumer) : AutoCloseable {
    /**
     * The next frame, gap, or end, or null once the track ends.
     *
     * @throws MoqException.Cancelled after [cancel], including a read already pending.
     */
    suspend fun next(): Read? =
        when (val read = inner.next()) {
            null -> null
            is MoqVideoRead.Frame -> ReadFrame(DecodedFrame(read.frame))
            is MoqVideoRead.Gap -> ReadGap(read.timestamp, read.duration)
            is MoqVideoRead.End -> ReadEnd(read.end)
        }

    /** Cancel all current and future reads, which then throw [MoqException.Cancelled]. */
    fun cancel() = inner.cancel()

    /** Release the native consumer. */
    override fun close() = inner.close()

    companion object {
        /** Subscribe to the catalog video rendition [id] and decode it. */
        suspend fun subscribe(
            broadcast: BroadcastConsumer,
            id: String,
            catalogVideo: dev.moq.media.Video,
            output: DecoderOutput,
        ): Consumer =
            Consumer(uniffi.moq.MoqVideoConsumer.subscribe(broadcast, id, catalogVideo, output))
    }
}

/** One item a [Consumer] reads, in delivery order: [ReadFrame], [ReadGap], or [ReadEnd]. */
sealed class Read

/** A decoded picture; `close()` its frame promptly, since held frames stall the decoder. */
class ReadFrame internal constructor(val frame: DecodedFrame) : Read()

/** Media after what was already read was lost; what follows does not continue it. */
data class ReadGap(
    /** The furthest presentation point already read. */
    val timestamp: Timestamp,
    /** An upper bound on how long until the next picture resumes. */
    val duration: java.time.Duration,
) : Read()

/** The publisher stopped its encoder; what follows comes from a fresh encoder. */
data class ReadEnd(
    /** Where the stopped encoder's media stops. */
    val end: Timestamp
) : Read()

/**
 * One decoded picture, owning the decoder's surface until closed; [pixels] converts it to packed
 * CPU pixels.
 */
class DecodedFrame internal constructor(private val inner: uniffi.moq.MoqVideoDecodedFrame) :
    AutoCloseable {
    /** The presentation timestamp. */
    fun timestamp(): Timestamp = inner.timestamp()

    /** The decoded width in pixels; `resize` is only a hint. */
    fun width(): UInt = inner.width()

    /** The decoded height in pixels. */
    fun height(): UInt = inner.height()

    /** The picture converted to tightly packed CPU pixels in [format]. */
    fun pixels(format: PixelFormat): ByteArray = inner.pixels(format)

    /** The decoder's platform surface, or null when the frame is in CPU memory. */
    fun surface(): Surface? = inner.surface()?.let(Surface::from)

    /** Release the frame's surface back to the decoder. */
    override fun close() = inner.close()
}

/**
 * The platform image backing a [DecodedFrame], when [DecoderOutput] asked for one. Match it with an
 * `else` branch, since new platforms add kinds.
 */
abstract class Surface internal constructor() {
    /** A borrowed `CVPixelBufferRef` address (macOS and iOS), valid while its frame lives. */
    data class PixelBuffer(val pointer: ULong) : Surface()

    /** A surface kind this version of the package does not know yet. */
    data object Unknown : Surface()

    internal companion object {
        /** Maps a generated surface, so a variant this package predates is [Unknown]. */
        fun from(surface: MoqVideoSurface): Surface =
            (surface as? MoqVideoSurface.PixelBuffer)?.let { PixelBuffer(it.pointer) } ?: Unknown
    }
}
