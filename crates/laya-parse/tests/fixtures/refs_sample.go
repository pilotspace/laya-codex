package cache

import (
	"fmt"
	"github.com/acme/moon/store"
)

type Cache struct {
	inner *store.Store
	meta  Metadata
}

func (c *Cache) Lookup(key string) (Entry, error) {
	raw, err := c.inner.FetchRaw(key)
	if err != nil {
		return Entry{}, fmt.Errorf("lookup: %w", err)
	}
	return decode(raw), nil
}
