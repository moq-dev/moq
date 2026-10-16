package dev.moq

/**
 * The settings a default [ClientConfig] dials with, for a settings UI to show.
 *
 * Knobs whose default depends on the backend (`tls.systemRoots`, `quic.gso`,
 * `quic.mtuDiscovery`, `quic.congestionControl`) stay null.
 */
fun clientDefaults(): ClientConfig = uniffi.moq.moqClientDefaults()

/** The protocol versions offered by default, most preferred first, spelled the way [ClientConfig.versions] takes them. */
fun versions(): List<String> = uniffi.moq.moqVersions()

/** Whether this build can write qlog traces for [QuicConfig.qlog]. */
fun qlogSupported(): Boolean = uniffi.moq.moqQlogSupported()
