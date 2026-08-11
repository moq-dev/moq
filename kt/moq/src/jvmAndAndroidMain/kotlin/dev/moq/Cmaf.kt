package dev.moq

import uniffi.moq.MoqCmafConfig
import uniffi.moq.MoqCmafMuxer
import uniffi.moq.MoqCmafTrack

/** Initialization and media data produced for one frame batch. */
data class CmafOutput(
    /** The current initialization segment once codec metadata is available. */
    val initialization: ByteArray?,
    /** The media fragment, or null when the batch produced no usable samples. */
    val fragment: ByteArray?,
)

/** Packages one encoded audio or video rendition as CMAF. */
class CmafMuxer private constructor(private val ffi: MoqCmafMuxer) : AutoCloseable {
    /** Creates a video muxer whose output timestamps are relative to [originUs]. */
    constructor(video: Video, originUs: ULong = 0uL) : this(
        MoqCmafMuxer(MoqCmafConfig(MoqCmafTrack.Video(video), originUs)),
    )

    /** Creates an audio muxer whose output timestamps are relative to [originUs]. */
    constructor(audio: Audio, originUs: ULong = 0uL) : this(
        MoqCmafMuxer(MoqCmafConfig(MoqCmafTrack.Audio(audio), originUs)),
    )

    /** The current initialization, or null until inline codec metadata arrives. */
    val initialization: ByteArray?
        get() = ffi.initSegment()

    /** Encodes one codec-configuration-consistent batch on the configured presentation timeline. */
    fun mux(sequence: UInt, frames: List<MediaFrame>): CmafOutput {
        val output = ffi.mux(sequence, frames)
        return CmafOutput(output.initialization, output.fragment)
    }

    /** Releases the native muxer. */
    override fun close() = ffi.close()
}
