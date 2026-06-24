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

// NewCache creates a cache with the given configuration. Use the zero CacheConfig for the default
// 64 MiB / 5s budget.
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
