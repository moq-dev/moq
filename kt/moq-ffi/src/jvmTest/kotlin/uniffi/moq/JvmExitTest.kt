package uniffi.moq

import com.sun.jna.CallbackThreadInitializer
import com.sun.jna.Native
import java.nio.file.Path
import java.util.Collections
import java.util.concurrent.CompletableFuture
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.concurrent.thread
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking

class JvmExitTest {
    @Test
    fun `main can return inside continuations on both runtime workers`() {
        for (backtrace in listOf(false, true)) {
            val lines = child(backtrace, daemon = false)
            assertEquals(2, lines.count { it == "callback-entered:daemon=false" }, lines.joinToString("\n"))
            assertEquals(2, lines.count { it == "callback-returning" }, lines.joinToString("\n"))
            val returned = lines.indexOf("main-returned")
            assertTrue(returned >= 0)
            assertTrue(lines.withIndex().filter { it.value == "callback-returning" }.all { it.index > returned })
        }
    }

    @Test
    fun `daemon continuation control exits before returning to Rust`() {
        val lines = child(backtrace = false, daemon = true)
        assertEquals(2, lines.count { it == "callback-entered:daemon=true" }, lines.joinToString("\n"))
        assertFalse(lines.contains("callback-returning"), lines.joinToString("\n"))
    }

    private fun child(backtrace: Boolean, daemon: Boolean): List<String> {
        val builder = ProcessBuilder(
            Path.of(System.getProperty("java.home"), "bin", "java").toString(),
            "-cp", System.getProperty("moq.test.classpath"),
            ExitDuringContinuation::class.java.name,
            daemon.toString(),
        ).redirectErrorStream(true)
        builder.environment()["TOKIO_WORKER_THREADS"] = "2"
        builder.environment().remove("RUST_BACKTRACE")
        if (backtrace) builder.environment()["RUST_BACKTRACE"] = "1"
        val process = builder.start()
        val lines = Collections.synchronizedList(mutableListOf<String>())
        val mainReturned = CompletableFuture<Unit>()
        val output = thread(isDaemon = true, name = "jvm-exit-output") {
            process.inputStream.bufferedReader().useLines { input ->
                input.forEach { line ->
                    lines.add(line)
                    if (line == "main-returned") mainReturned.complete(Unit)
                }
            }
            mainReturned.completeExceptionally(IllegalStateException("child exited before main returned: $lines"))
        }
        try {
            if (!daemon) {
                // The daemon controller joins main before announcing this marker. Both native
                // callbacks remain attached and gated until the parent explicitly releases them.
                mainReturned.get(30, TimeUnit.SECONDS)
                process.outputStream.write('R'.code)
                process.outputStream.flush()
            }
            assertTrue(process.waitFor(30, TimeUnit.SECONDS), "child JVM did not exit: $lines")
            output.join()
            assertEquals(0, process.exitValue(), lines.joinToString("\n"))
            assertFalse(lines.any { it.contains("panicked", ignoreCase = true) }, lines.joinToString("\n"))
            return lines.toList()
        } finally {
            process.outputStream.close()
            if (process.isAlive) process.destroyForcibly().waitFor()
            output.join()
        }
    }
}

/** A raw UniFFI child, deliberately leaving its live handles to process exit. */
internal object ExitDuringContinuation {
    // Keep JNA callbacks and native handles alive until VM shutdown, including after main returns.
    private val roots = mutableListOf<Any>()

    @JvmStatic
    fun main(args: Array<String>) {
        val daemon = args.single().toBooleanStrict()
        val main = Thread.currentThread()
        val entered = CountDownLatch(2)
        val release = CountDownLatch(1)
        if (daemon) {
            Native.setCallbackThreadInitializer(uniffiRustFutureContinuationCallbackImpl, CallbackThreadInitializer(true))
        }
        val scope = CoroutineScope(Dispatchers.Unconfined)
        roots.add(scope)

        val servers = List(2) {
            MoqServer(MoqServerConfig(bind = "127.0.0.1:0", tls = MoqServerTls(generate = listOf("localhost"))))
        }
        roots.addAll(servers)
        val addresses = runBlocking { servers.map { it.listen() } }
        // Unconfined resumes inline inside the actual generated continuation callback. Socket
        // readiness wakes it on a native worker, and the gate keeps that JNA callback in flight.
        roots.addAll(servers.map { server ->
            scope.launch {
                checkNotNull(server.accept())
                check(Thread.currentThread() !== main) { "continuation must run on a native runtime worker" }
                println("callback-entered:daemon=${Thread.currentThread().isDaemon}")
                entered.countDown()
                release.await()
                println("callback-returning")
            }
        })
        val clients = List(2) {
            MoqClient(MoqClientConfig(bind = "127.0.0.1:0", tls = MoqClientTls(insecure = true), once = true))
        }
        roots.addAll(clients)
        roots.addAll(clients.zip(addresses).map { (client, address) ->
            scope.launch { client.connect("https://$address") }
        })
        entered.await()
        thread(isDaemon = true, name = "jvm-exit-controller") {
            main.join()
            println("main-returned")
            check(System.`in`.read() == 'R'.code) { "missing parent release" }
            release.countDown()
        }
        println("main-returning")
    }
}
