// Subscribe-only cross-language interop client for the smoke test, built on the
// published Kotlin package (dev.moq:moq on Maven Central). Connect, find the
// video track in the catalog, and exit 0 as soon as any non-empty frame arrives
// (1 on timeout).
//
//   smoke subscribe --url http://127.0.0.1:4443 --broadcast b.hang --timeout 20
//
// Publishing isn't wired up: the raw-stream importer the other clients publish
// with isn't in the published 0.2.x FFI yet, so this client only subscribes.
import dev.moq.frames
import dev.moq.updates
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.mapNotNull
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import uniffi.moq.MoqBroadcastConsumer
import uniffi.moq.MoqClient
import uniffi.moq.MoqOriginProducer
import uniffi.moq.MoqVideo
import kotlin.system.exitProcess

private const val MAX_LATENCY_MS = 1_000uL

// A catalog update that actually carries a video track. A lazy publisher may
// announce video in a later update, not the first snapshot.
private suspend fun videoTrack(bc: MoqBroadcastConsumer): Pair<String, MoqVideo> =
    bc.subscribeCatalog().use { catalog ->
        catalog.updates()
            .mapNotNull { it.video.entries.firstOrNull() }
            .first()
            .let { it.key to it.value }
    }

private suspend fun subscribe(url: String, broadcast: String) {
    val origin = MoqOriginProducer()
    val client = MoqClient()
    client.setTlsDisableVerify(true)
    client.setConsume(origin)

    val session = client.connect(url)
    try {
        val consumer = origin.consume()
        val announced = consumer.announcedBroadcast(broadcast)
        val bc = announced.available()

        val (name, video) = videoTrack(bc)
        val media = bc.subscribeMedia(name, video.container, MAX_LATENCY_MS)

        // Suspends until the first non-empty frame, or throws if the flow ends.
        val frame = media.frames().first { it.payload.isNotEmpty() }
        System.err.println("received ${frame.payload.size} bytes from $broadcast")
    } finally {
        session.cancel(0u) // code 0 = graceful close
    }
}

fun main(args: Array<String>) {
    var url = ""
    var broadcast = ""
    var timeout = 20.0
    var i = 0
    while (i < args.size) {
        when (args[i]) {
            "--url" -> url = args.getOrElse(++i) { "" }
            "--broadcast" -> broadcast = args.getOrElse(++i) { "" }
            "--timeout" -> timeout = args.getOrElse(++i) { "20" }.toDoubleOrNull() ?: 20.0
            // leading "subscribe" positional and anything else ignored
        }
        i++
    }
    if (url.isEmpty() || broadcast.isEmpty()) {
        System.err.println("usage: smoke subscribe --url U --broadcast B [--timeout S]")
        exitProcess(2)
    }

    val code = runBlocking {
        try {
            withTimeout((timeout * 1000).toLong()) { subscribe(url, broadcast) }
            0
        } catch (e: TimeoutCancellationException) {
            System.err.println("error: timed out waiting for data")
            1
        } catch (e: Exception) {
            System.err.println("error: ${e.message}")
            1
        }
    }
    exitProcess(code)
}
