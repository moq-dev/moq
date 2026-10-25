package dev.moq

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.onCompletion
import uniffi.moq.MoqException

/**
 * Stream of decoded audio frames in the layout declared by the
 * [AudioDecoderOutput] the consumer was created with.
 */
fun dev.moq.audio.Consumer.frames(): Flow<dev.moq.audio.Frame> = pull({ cancel() }) { next() }

/**
 * Stream of decoded video frames. Each owns the decoder's surface: close it
 * (or `use` it) when done, since held frames stall the decoder.
 */
fun dev.moq.video.Consumer.frames(): Flow<VideoDecodedFrame> = pull({ cancel() }) { next() }

/** Stream of groups in sequence order, skipping forward if the reader falls behind. */
fun TrackConsumer.groups(): Flow<GroupConsumer> = pull({ cancel() }) { nextGroup() }

/** Stream of groups in arrival order, including out-of-sequence deliveries. */
fun TrackConsumer.groupsAsArrived(): Flow<GroupConsumer> = pull({ cancel() }) { recvGroup() }

/**
 * Stream of timestamped raw frames from one-frame-per-group tracks.
 *
 * Completed empty groups are skipped. The flow ends when the track ends.
 */
fun TrackConsumer.frames(): Flow<Frame> = pull({ cancel() }) { readFrame() }

/** Stream of best-effort datagrams in arrival order. */
fun TrackConsumer.datagrams(): Flow<Datagram> = pull({ cancel() }) { recvDatagram() }

/** Stream of tracks requested by subscribers. */
fun BroadcastRequests.requestedTracks(): Flow<TrackRequest> = pull({ cancel() }) { requestedTrack() }

/** Stream of the fills readers of one track need. */
fun TrackRequests.requestedFills(): Flow<Fill> = pull({ cancel() }) { requestedFill() }

/** Stream of broadcasts requested by consumers. */
fun OriginRequests.requestedBroadcasts(): Flow<BroadcastRequest> = pull({ cancel() }) { requestedBroadcast() }

/** Stream of timestamped raw frames within a group. */
fun GroupConsumer.frames(): Flow<Frame> = pull({ cancel() }) { readFrame() }

/**
 * Stream of announce events, as `announced(config).updates()`.
 *
 * Collect it once: the handle is cancelled when collection ends, however it ends.
 */
fun AnnounceConsumer.updates(): Flow<AnnounceEvent> = pull { next() }.onCompletion { cancel() }

/**
 * The session's grant: the current one, then each change, null while there is none.
 *
 * Collections of one [Auth] share its place in the changes, so a second watcher takes
 * its own from [Moq.auth]. Completes when the session shuts down, and throws its close reason otherwise.
 */
fun Auth.grants(): Flow<Grant?> = flow {
    emit(grant())
    while (true) {
        currentCoroutineContext().ensureActive()
        val grant = try {
            grantChanged()
        } catch (e: MoqException) {
            if (e.isShutdown) break
            throw e
        }
        emit(grant)
    }
}

/** Emits [next] until it returns null, checking for cancellation before each pull. */
private fun <T : Any> pull(next: suspend () -> T?): Flow<T> = flow {
    while (true) {
        currentCoroutineContext().ensureActive()
        emit(next() ?: break)
    }
}

/**
 * [pull], calling [cancel] when the collector is cancelled so the native
 * handle wakes a pending [next] and releases the stream.
 */
internal fun <T : Any> pull(cancel: () -> Unit, next: suspend () -> T?): Flow<T> =
    pull(next).onCompletion { cause -> if (cause is CancellationException) cancel() }
