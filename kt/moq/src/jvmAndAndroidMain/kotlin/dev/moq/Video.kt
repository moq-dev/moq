package dev.moq

// Constructors for [VideoEncoderKind]. It is a sealed class, whose subtypes
// Kotlin 2.0.21 cannot reach through the typealias in Aliases.kt
// (`VideoEncoderKind.Software` does not resolve), so these keep the whole
// publish surface inside `dev.moq` rather than sending callers to `uniffi.moq`
// for one argument.

/** Prefer a platform hardware encoder, falling back to software. */
val autoEncoder: VideoEncoderKind = uniffi.moq.MoqVideoEncoderKind.Auto

/** Hardware only; fails if none is available. */
val hardwareEncoder: VideoEncoderKind = uniffi.moq.MoqVideoEncoderKind.Hardware

/** Software only (openh264, H.264 only). */
val softwareEncoder: VideoEncoderKind = uniffi.moq.MoqVideoEncoderKind.Software

/**
 * A specific backend these bindings compile: `"videotoolbox"` (macOS),
 * `"mediafoundation"` (Windows), or `"openh264"` (software, everywhere).
 * Naming one this build lacks fails with a no-encoder error.
 */
fun namedEncoder(name: String): VideoEncoderKind = uniffi.moq.MoqVideoEncoderKind.Named(name)
