package dev.moq

/** The protocol versions offered by default, most preferred first, spelled the way [ClientConfig.versions] takes them. */
fun versions(): List<String> = uniffi.moq.moqVersions()

/** Whether this build can write qlog traces for [QuicConfig.qlog]. */
fun qlogSupported(): Boolean = uniffi.moq.moqQlogSupported()
