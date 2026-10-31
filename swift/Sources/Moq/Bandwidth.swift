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

/// What a sender following a ``RateControl`` should be doing.
public typealias RateTarget = MoqRateTarget

/// Turns a ``Reservation``'s grant into a sender's ``RateTarget``.
///
/// The same policy moq's own encoders follow: drops apply at once, raises climb at most 25% per
/// second, and moves under 5% are ignored. For a sender that encodes on its own, such as a
/// pre-encoded track: call `update(_:)` on every frame and apply what it returns.
///
/// Pass `lowerBps`, the next lower rendition's `maxBps`, for a rendition encoded beside lower
/// ones of the same source: a grant below `(maxBps + 2 * lowerBps) / 3` then disables it while
/// a session probes for the reservation.
public final class RateControl: Sendable {
    let ffi: MoqRateControl

    public init(maxBps: UInt64, lowerBps: UInt64? = nil) {
        ffi = MoqRateControl(policy: MoqRatePolicy(maxBps: maxBps, lowerBps: lowerBps))
    }

    /// What the sender should currently be doing.
    public func target() -> RateTarget {
        ffi.target()
    }

    /// Feed `reservation`'s grant, returning a new target worth applying, or nil.
    public func update(_ reservation: Reservation) -> RateTarget? {
        ffi.update(reservation: reservation.ffi)
    }
}
