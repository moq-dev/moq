package dev.moq

/** Configure the native tracing level, such as `info`, `debug`, or `trace`. */
fun logLevel(level: String = "info") {
    uniffi.moq.moqLogLevel(level)
}
