import MoqFFI

/// Divides one connection's send estimate among the tracks sharing it.
///
/// Minted by `Session.bandwidth`. Handles from the same session share one
/// reservation registry.
public final class Bandwidth: Sendable {
    let ffi: MoqBandwidth

    init(_ ffi: MoqBandwidth) {
        self.ffi = ffi
    }

    /// Reserve up to `maxBps` for `track`.
    ///
    /// `maxBps` is a ceiling, not a measurement: reserve the most the track can
    /// ever send. Drop the reservation to hand the room back.
    public func reserve(track: TrackProducer, maxBps: UInt64) throws -> Reservation {
        Reservation(try ffi.reserve(track: track.ffi, maxBps: maxBps))
    }
}

/// One track's standing claim on a `Bandwidth`.
///
/// `grant()` is a snapshot: `nil` means no estimate or no demand, so hold the
/// current rate, and `0` is a real zero grant.
public final class Reservation: Sendable {
    let ffi: MoqReservation

    init(_ ffi: MoqReservation) {
        self.ffi = ffi
    }

    /// This reservation's slice right now, in bits per second.
    public func grant() -> UInt64? {
        ffi.grant()
    }

    /// Change the ceiling, keeping the same claim.
    public func update(maxBps: UInt64) {
        ffi.update(maxBps: maxBps)
    }
}
