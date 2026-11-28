import Foundation
import MoqFFI

// The generated Duration converter aborts the process on a negative, NaN, or oversized
// `TimeInterval`, so each public entry that takes one checks it first and throws
// `MoqError.Config`, as the Kotlin and Python bindings do.

/// Throw `MoqError.Config` for a duration the generated converter would abort on.
func checkDuration(_ value: TimeInterval?, _ name: String) throws {
    guard let value else { return }
    // The converter's own bound: NaN fails the first comparison, infinity the second.
    guard value >= 0, value.rounded(.down) <= Double(Int64.max) else {
        throw MoqError.Config(reason: "invalid \(name): \(value) seconds, want a finite non-negative duration")
    }
}

func checkDelay(_ delay: Delay?, _ name: String = "delay") throws {
    guard let delay else { return }
    try checkDuration(delay.min, "\(name).min")
    try checkDuration(delay.max, "\(name).max")
}

func checkSubscription(_ subscription: Subscription?) throws {
    try checkDelay(subscription?.delay)
}

func checkTrackInfo(_ info: TrackInfo?) throws {
    try checkDuration(info?.maxAge, "maxAge")
    try checkDuration(info?.refresh.interval, "refresh.interval")
}

func checkQuic(_ quic: QuicConfig) throws {
    try checkDuration(quic.idleTimeout, "quic.idleTimeout")
    try checkDuration(quic.keepAlive, "quic.keepAlive")
}

func checkBackoff(_ backoff: Backoff) throws {
    try checkDuration(backoff.initial, "backoff.initial")
    try checkDuration(backoff.max, "backoff.max")
    try checkDuration(backoff.timeout, "backoff.timeout")
}

func checkAudio(_ audio: Media.Audio) throws {
    try checkDuration(audio.jitter, "jitter")
    try checkDuration(audio.warmup, "warmup")
}

func checkRendition(_ rendition: Media.Rendition) throws {
    if case .audio(_, let config) = rendition {
        try checkAudio(config)
    }
}
