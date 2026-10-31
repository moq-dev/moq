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
/// `rate()` is a snapshot: `nil` means no estimate, so hold the current rate,
/// and `0` is a real zero rate. While nothing subscribes it is the share the
/// track would get.
public final class Reservation: Sendable {
    let ffi: MoqReservation

    init(_ ffi: MoqReservation) {
        self.ffi = ffi
    }

    /// This reservation's slice right now, in bits per second.
    public func rate() -> UInt64? {
        ffi.rate()
    }

    /// Change the ceiling, keeping the same claim.
    public func update(maxBps: UInt64) {
        ffi.update(maxBps: maxBps)
    }
}

/// Divides one connection's receive estimate among the broadcasts watched over it.
///
/// Minted by `Session.viewers`. Handles from the same session share one registry.
public final class Viewers: Sendable {
    let ffi: MoqViewers

    init(_ ffi: MoqViewers) {
        self.ffi = ffi
    }

    /// Watch a broadcast receiving at most `maxBps`, returning its viewer.
    ///
    /// `maxBps` is a ceiling: what the largest rendition the player would switch to
    /// needs, not what it is receiving. Drop the viewer once the broadcast stops being
    /// watched. Holding one requests the receive estimate, like `Session.recvBandwidth()`.
    public func watch(maxBps: UInt64) -> Viewer {
        Viewer(ffi.watch(maxBps: maxBps))
    }

    /// Send `targetBps` as the PROBE target instead of the viewers' sum, or `nil` to
    /// follow them again. `0` sends no target.
    public func setTarget(targetBps: UInt64?) {
        ffi.setTarget(targetBps: targetBps)
    }
}

/// One watched broadcast's standing claim on `Viewers`.
///
/// `rate()` is a snapshot: `nil` means no receive estimate, so hold the current
/// selection, and `0` is a real zero rate.
public final class Viewer: Sendable {
    let ffi: MoqViewer

    init(_ ffi: MoqViewer) {
        self.ffi = ffi
    }

    /// This viewer's slice of the receive estimate right now, in bits per second.
    public func rate() -> UInt64? {
        ffi.rate()
    }

    /// Change the ceiling, keeping the same claim.
    public func update(maxBps: UInt64) {
        ffi.update(maxBps: maxBps)
    }
}
