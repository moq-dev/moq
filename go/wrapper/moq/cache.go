package moq

import ffi "github.com/moq-dev/moq-go-ffi/moq"

// Cache is a shared, cheaply cloneable handle to a RAM LRU group cache.
//
// Attach it to a broadcast (or origin) with WithCache; pass the same Cache to several to pool one
// retention budget across them. Eviction is LRU by wall-clock last-access time, bounded by a byte
// budget and an age window.
type Cache struct {
	inner *ffi.MoqCache
}

// DefaultCacheMaxAgeMs is the default retention window (5 seconds), matching moq-net's
// DEFAULT_CACHE. It is what a broadcast gets when no cache is attached.
const DefaultCacheMaxAgeMs uint64 = 5000

// DefaultCacheConfig returns the default cache configuration: a 5-second window and no byte cap.
//
// Prefer this over a zero-valued CacheConfig: a zero MaxAgeMs means a zero retention window
// (latest group only), not the 5-second default.
func DefaultCacheConfig() CacheConfig {
	return CacheConfig{MaxBytes: 0, MaxAgeMs: DefaultCacheMaxAgeMs}
}

// NewCache creates a cache with the given configuration. A MaxBytes of 0 means no byte cap; a
// MaxAgeMs of 0 means a zero retention window (latest group only). For the standard 5-second
// window use DefaultCacheConfig.
func NewCache(config CacheConfig) *Cache {
	return &Cache{inner: ffi.NewMoqCache(config)}
}

// CloneHandle returns a handle sharing this cache's budget. Attach it elsewhere to pool retention.
func (c *Cache) CloneHandle() *Cache {
	return &Cache{inner: c.inner.CloneHandle()}
}

// IsClone reports whether two handles share the same underlying budget.
func (c *Cache) IsClone(other *Cache) bool {
	return c.inner.IsClone(other.inner)
}
