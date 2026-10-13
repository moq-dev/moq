import Foundation
import MoqFFI

/// Mint a fresh publisher epoch from the wall clock and secure randomness,
/// ordered newest last. Mint one per publisher run and announce it in
/// `Route.epoch`, so viewers see a restart as a new broadcast instead of a
/// stalled one.
public func mintEpoch() -> String {
    moqMintEpoch()
}

/// The wall-clock time an epoch encodes, to the millisecond. Throws unless
/// `epoch` is a lowercase hyphenated UUIDv7.
public func epochTime(_ epoch: String) throws -> Date {
    let ms = try moqEpochTimeMs(epoch: epoch)
    return Date(timeIntervalSince1970: Double(ms) / 1000)
}
