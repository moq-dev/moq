package bench

import java.io.File
import java.lang.management.ManagementFactory
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout

// Kotlin runner for the generator comparison (see ../../README.md). The same rounds as
// py/bench.py, written against the names each generator's Gen.kt aliases.

private const val WARMUP = 3
private const val CACHE_BYTES: ULong = 67_108_864uL
private const val ROUND_TIMEOUT_MS = 30_000L
private val SIZES = listOf(100, 1024, 16 * 1024, 64 * 1024, 1024 * 1024)
private val budgetNs = ((System.getenv("MOQ_BENCH_SECONDS")?.toDouble() ?: 1.0) * 1e9).toLong()

private fun cpuNs(): Long =
    (ManagementFactory.getOperatingSystemMXBean() as com.sun.management.OperatingSystemMXBean).processCpuTime

/** Frames per group, holding a group under moq-net's 32 MB group cache. */
private fun frames(size: Int) = ((8 shl 20) / size).coerceIn(1, 64)

private class Result(val name: String, val unit: String, val value: Double)

private suspend fun throughput(size: Int, results: MutableList<Result>) = coroutineScope {
    val origin = MoqOriginProducer(CACHE_BYTES)
    val broadcast = origin.createBroadcast("bench")
    val track = broadcast.publishTrack("0")
    val consumer = track.consume()
    // A non-uniform payload, so a zero-filled or truncated copy fails the check.
    val payload = ByteArray(size) { (it % 251).toByte() }
    val count = frames(size)

    suspend fun step(n: Long) {
        val reader = async {
            consumer.recvGroup().use { group ->
                repeat(count) { f ->
                    val frame = checkNotNull(group.readFrame()) { "group ended" }
                    check(frame.payload.contentEquals(payload)) { "corrupt frame $f of round $n" }
                    check(frame.timestampUs == (n * count + f).toULong())
                }
            }
        }
        track.appendGroup().use { group ->
            repeat(count) { f ->
                group.writeFrame(MoqFrame(payload, (n * count + f).toULong()))
            }
            group.finish()
        }
        withTimeout(ROUND_TIMEOUT_MS) { reader.await() }
    }

    repeat(WARMUP) { step(it.toLong()) }
    val (start, cpuStart) = System.nanoTime() to cpuNs()
    var n = WARMUP.toLong()
    while (System.nanoTime() - start < budgetNs) {
        step(n)
        n++
    }
    val (wallNs, cpuUsedNs) = (System.nanoTime() - start) to (cpuNs() - cpuStart)
    val mb = (n - WARMUP) * count.toLong() * size / 1e6
    results += Result("throughput/$size", "MB/s", mb / (wallNs / 1e9))
    results += Result("cpu/$size", "ms/MB", cpuUsedNs / 1e6 / mb)
    listOf(consumer, track, broadcast, origin).forEach { it.close() }
}

/** Time [fn] twice and keep the second, so the JIT has compiled the call path. */
private inline fun call(name: String, results: MutableList<Result>, fn: () -> Unit) {
    var perCall = 0.0
    repeat(2) {
        val start = System.nanoTime()
        var calls = 0L
        while (System.nanoTime() - start < budgetNs) {
            repeat(1000) { fn() }
            calls += 1000
        }
        perCall = (System.nanoTime() - start).toDouble() / calls
    }
    results += Result("call/$name", "ns", perCall)
}

/** A finished track surfaces as the slice's `Ended` error on both generators. */
private suspend fun ended() {
    MoqBroadcastProducer().use { broadcast ->
        val track = broadcast.publishTrack("ended")
        val consumer = track.consume()
        track.finish()
        val err = runCatching { consumer.recvGroup() }.exceptionOrNull()
        check(err != null && err::class.simpleName == "Ended") { "expected Ended, got $err" }
    }
}

fun main(args: Array<String>) {
    val results = mutableListOf<Result>()
    runBlocking {
        ended()
        SIZES.forEach { throughput(it, results) }
    }

    val origin = MoqOriginProducer(CACHE_BYTES)
    val track = origin.createBroadcast("calls").publishTrack("calls")
    var group = track.appendGroup()
    val consumer = track.consume()
    call("noop", results) { noop() }
    call("sequence", results) { group.sequence() }
    call("info", results) { consumer.info() }

    // A fresh group every 1000 frames, so the origin's cache can evict the old ones.
    val frame = MoqFrame(ByteArray(100), 0uL)
    var writes = 0L
    call("write_frame/100", results) {
        if (++writes % 1000 == 0L) {
            group.finish()
            group.close()
            group = track.appendGroup()
        }
        group.writeFrame(frame)
    }
    runBlocking {
        call("noop_async", results) { noopAsync() }
        call("yield_once", results) { yieldOnce() }
    }

    val json = results.joinToString(",\n    ", "[\n    ", "\n  ]") {
        """{"name": "${it.name}", "unit": "${it.unit}", "value": ${it.value}}"""
    }
    File(args[0]).writeText("""{"runner": "kotlin-${System.getProperty("bench.generator")}", "results": $json}""")
}
