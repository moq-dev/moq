package dev.moq

import java.io.File
import java.lang.management.ManagementFactory
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.put
import kotlinx.serialization.json.addJsonObject
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

/** One throughput case: [publishers] tracks, each read by [subscribers] consumers. */
private data class Case(val size: Int, val publishers: Int, val subscribers: Int) {
    /** Frames per group, holding a group under moq-net's 32 MB group cache. */
    val frames = ((8 shl 20) / size).coerceIn(1, 64)
    val delivered = publishers.toLong() * subscribers * frames * size
    val name = "size=$size/pub=$publishers/sub=$subscribers"
}

// Frame sizes at one publisher and subscriber, then fan-out at 1 KiB.
private val cases = listOf(100, 1024, 64 * 1024, 1024 * 1024).map { Case(it, 1, 1) } +
    listOf(1 to 4, 4 to 1, 4 to 4).map { (p, s) -> Case(1024, p, s) }

private const val WARMUP = 3

// Each case's origin cache budget: far above one round's groups, so eviction only
// reclaims groups every subscriber has read, and memory stays flat over a long run.
private const val CACHE_BYTES: ULong = 67_108_864uL // 64 MiB
private val budgetNs = ((System.getenv("MOQ_BENCH_SECONDS")?.toDouble() ?: 1.0) * 1e9).toLong()

// Far longer than any honest round, so only a lost wakeup trips it.
private const val ROUND_TIMEOUT_MS = 30_000L

/** Process CPU time, user plus system, across every thread. */
private fun cpuNs(): Long =
    (ManagementFactory.getOperatingSystemMXBean() as com.sun.management.OperatingSystemMXBean).processCpuTime

private class Result(val name: String, val unit: String, val value: Double)

/**
 * The Kotlin runner of the binding benchmark (see bench/README.md).
 *
 * The same in-process rounds as every wrapper, so dividing by the Rust baselines isolates
 * what Kotlin adds. Runs only when `MOQ_BENCH_DIR` is set, so `just kt check` skips it.
 */
class FfiBench {
    @Test
    fun benchmark() {
        val dir = System.getenv("MOQ_BENCH_DIR") ?: return
        val results = mutableListOf<Result>()
        runBlocking { cases.forEach { throughput(it, results) } }

        val broadcast = BroadcastProducer()
        val track = broadcast.publishTrack("calls", null)
        val producer = track.appendGroup()
        val group = producer.consume()
        call("sequence", results) { group.sequence() }
        val consumer = track.consume(null)
        call("info", results) { consumer.info() }
        listOf(consumer, group, producer, track, broadcast).forEach { it.close() }

        val json = buildJsonArray {
            results.forEach {
                addJsonObject {
                    put("name", it.name)
                    put("unit", it.unit)
                    put("value", it.value)
                }
            }
        }
        File(dir).mkdirs()
        File(dir, "kotlin.json").writeText(json.toString())
    }

    private suspend fun throughput(case: Case, results: MutableList<Result>) = kotlinx.coroutines.coroutineScope {
        // Close every handle inside the case, so native teardown never lands in a later one.
        val origin = OriginProducer(OriginConfig(cacheCapacityBytes = CACHE_BYTES))
        val publication = origin.createBroadcast("bench")
        val broadcast = publication.broadcast()
        val tracks = (0 until case.publishers).map { broadcast.publishTrack("$it", TrackInfo(timescale = 1_000_000uL)) }
        val consumers = tracks.flatMap { track -> (0 until case.subscribers).map { track.consume(null) } }
        // A non-uniform payload, so a zero-filled or truncated copy fails the check.
        val payload = ByteArray(case.size) { (it % 251).toByte() }

        // Each consumer reads on its own coroutine while this one writes each group in turn.
        suspend fun step(n: Long) {
            val readers = consumers.map { consumer ->
                async {
                    assertNotNull(consumer.recvGroup(), "track ended").use { group ->
                        repeat(case.frames) { f ->
                            val frame = assertNotNull(group.readFrame(), "group ended")
                            assertTrue(frame.payload.contentEquals(payload), "corrupt frame $f of round $n")
                            assertEquals((n * case.frames + f).toULong(), frame.timestampUs)
                        }
                    }
                }
            }
            tracks.forEach { track ->
                track.appendGroup().use { group ->
                    repeat(case.frames) { f ->
                        group.writeFrame(Frame(payload = payload, timestampUs = (n * case.frames + f).toULong()))
                    }
                    group.finish()
                }
            }
            withTimeout(ROUND_TIMEOUT_MS) { readers.awaitAll() }
        }

        repeat(WARMUP) { step(it.toLong()) }
        val (start, cpuStart) = System.nanoTime() to cpuNs()
        var n = WARMUP.toLong()
        while (System.nanoTime() - start < budgetNs) {
            step(n)
            n++
        }
        val (wallNs, cpuUsedNs) = (System.nanoTime() - start) to (cpuNs() - cpuStart)
        val mb = (n - WARMUP) * case.delivered / 1e6
        results += Result("throughput/${case.name}", "MB/s", mb / (wallNs / 1e9))
        results += Result("cpu/${case.name}", "ms/MB", cpuUsedNs / 1e6 / mb)
        (consumers + tracks + listOf(broadcast, publication, origin)).forEach { it.close() }
    }

    private inline fun call(name: String, results: MutableList<Result>, fn: () -> Unit) {
        val start = System.nanoTime()
        var calls = 0L
        while (System.nanoTime() - start < budgetNs) {
            repeat(1000) { fn() }
            calls += 1000
        }
        results += Result("call/$name", "ns", (System.nanoTime() - start).toDouble() / calls)
    }
}
