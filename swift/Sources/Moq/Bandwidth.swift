import MoqFFI

/// An owned bandwidth estimate, in bits per second, returned by `Session.recvBandwidth()`.
public final class BandwidthConsumer: Sendable {
    let ffi: MoqBandwidthConsumer

    init(_ ffi: MoqBandwidthConsumer) { self.ffi = ffi }

    /// The current estimate, or nil while unavailable.
    public func rate() -> UInt64? { ffi.rate() }

    /// Wait for the estimate to change and return the new rate, throwing when its producer closes.
    public func rateChanged() async throws -> UInt64? { try await ffi.rateChanged() }

    /// Release this consumer's demand, including a pending wait.
    public func cancel() { ffi.cancel() }
}

/// Divides one connection's send estimate among the tracks sharing it.
///
/// Minted by `Session.allocator`. Handles from the same session share one
/// reservation registry.
public final class Allocator: Sendable {
    let ffi: MoqAllocator

    init(_ ffi: MoqAllocator) {
        self.ffi = ffi
    }

    /// Reserve up to `maxBps` for the track `demand` watches.
    ///
    /// Every track producer hands out its `demand()`. `maxBps` is a ceiling, not a measurement: reserve the most the track can
    /// ever send. Drop the reservation to hand the room back.
    public func reserve(demand: TrackDemand, maxBps: UInt64) -> Reservation {
        Reservation(ffi.reserve(demand: demand.ffi, maxBps: maxBps))
    }
}

/// One track's standing claim on an `Allocator`.
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
