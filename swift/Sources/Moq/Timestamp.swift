import MoqFFI

// The bounds moq-ffi enforces on a write, so a conversion refuses what a write would.
private let maxValue: UInt64 = (1 << 61) - 1
private let maxScale: UInt64 = (1 << 62) - 1
private let attosecondsPerSecond: UInt64 = 1_000_000_000_000_000_000

@available(macOS 13, iOS 16, *)
extension Timestamp {
    /// The timestamp exactly equal to `duration`, at the finest power-of-ten scale that fits.
    ///
    /// `Duration` counts attoseconds, so the scale starts at 10^18 and steps down one power of ten
    /// each time the value would pass `2^61 - 1` (about 2.3 s of attoseconds, 73 years of
    /// nanoseconds), down to whole seconds. Throws `MoqError.TimeOverflow` for a negative duration,
    /// or one with finer precision than the largest scale whose value fits.
    public init(_ duration: Duration) throws {
        guard duration >= .zero else { throw MoqError.TimeOverflow }
        let (seconds, attoseconds) = duration.components
        var scale = attosecondsPerSecond
        var divisor: UInt64 = 1
        while scale > 0 {
            let (whole, overflow) = UInt64(seconds).multipliedReportingOverflow(by: scale)
            let (fraction, remainder) = UInt64(attoseconds).quotientAndRemainder(dividingBy: divisor)
            // `fraction` is below `scale`, which is below `maxValue`, so the subtraction can't wrap.
            if !overflow, whole <= maxValue - fraction {
                // A coarser scale can't hold what this one drops, so stop here either way.
                guard remainder == 0 else { throw MoqError.TimeOverflow }
                self.init(value: whole + fraction, scale: scale)
                return
            }
            scale /= 10
            divisor *= 10
        }
        throw MoqError.TimeOverflow
    }

    /// This timestamp as a `Duration`, rounded down to a whole attosecond.
    ///
    /// Exact when `scale` divides 10^18, such as microseconds or nanoseconds; a 90 kHz tick rounds,
    /// so keep the record itself to republish a read exactly. Throws `MoqError.TimeOverflow` for a
    /// record outside the bounds a write accepts.
    public var duration: Duration {
        get throws {
            guard scale != 0, scale <= maxScale, value <= maxValue else { throw MoqError.TimeOverflow }
            let (seconds, remainder) = value.quotientAndRemainder(dividingBy: scale)
            // `remainder` is below `scale`, so the quotient is below 10^18 and fits.
            let (attoseconds, _) = scale.dividingFullWidth(remainder.multipliedFullWidth(by: attosecondsPerSecond))
            return Duration(secondsComponent: Int64(seconds), attosecondsComponent: Int64(attoseconds))
        }
    }
}
