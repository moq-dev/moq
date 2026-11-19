package dev.moq

/**
 * The protocol versions offered by default, most preferred first, spelled the way
 * [ClientConfig.versions] takes them.
 */
fun versions(): List<String> = uniffi.moq.moqVersions()

/**
 * The caller's product tokens, if any, then this package's, which moq-ffi advertises ahead of its
 * own.
 */
internal fun implementation(prefix: String?): String =
    listOfNotNull(prefix, "moq-kt/$VERSION").joinToString(" ")
