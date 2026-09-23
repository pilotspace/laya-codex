package store

import (
	"errors"
	"sync"
)

const MaxKeys = 1024

// ErrMissing is returned for unknown keys.
var ErrMissing = errors.New("missing")

// Store is a thread-safe map.
type Store struct {
	mu   sync.RWMutex
	data map[string]string
}

// Get returns a value.
func (s *Store) Get(key string) (string, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	v, ok := s.data[key]
	if !ok {
		return "", ErrMissing
	}
	return v, nil
}

// New builds a store.
func New() *Store {
	return &Store{data: map[string]string{}}
}
