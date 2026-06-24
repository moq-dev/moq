import MoqFFI

/// A shared, cheaply cloneable handle to a RAM LRU group cache.
///
/// Attach it to a broadcast (or origin) with `withCache`; pass the same `Cache` to several to
/// pool one retention budget across them. Eviction is LRU by wall-clock last-access time, bounded
/// by a byte budget and an age window.
public final class Cache: Sendable {
    let ffi: MoqCache

    /// Create a cache with the given configuration (defaults to a 64 MiB / 5s budget).
    public init(config: CacheConfig = CacheConfig()) {
        ffi = MoqCache(config: config)
    }

    init(_ ffi: MoqCache) {
        self.ffi = ffi
    }

    /// A handle sharing this cache's budget. Attach it elsewhere to pool retention.
    public func cloneHandle() -> Cache {
        Cache(ffi.cloneHandle())
    }

    /// Whether two handles share the same underlying budget.
    public func isClone(_ other: Cache) -> Bool {
        ffi.isClone(other: other.ffi)
    }
}
