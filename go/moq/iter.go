package moq

// Ergonomic range-over-func iterators layered on top of the generated pull-based
// `Next()` handles. uniffi can't carry a stream across the FFI boundary, so each
// stream is exposed as a `Next()`-style method; these adapters turn that into the
// idiomatic Go form `for x, err := range consumer.Xs()`.
//
// Iteration stops on the first error (yielded once with the zero element) or when
// the source closes (`Next()` returns nil). An iterator only tears down resources
// it created itself: `Announcements` cancels the subscription it acquired, while
// the consumer-owned iterators leave the consumer intact so the caller controls
// its lifetime.

import "iter"

// Announcements streams broadcast announcements under prefix. It acquires the
// subscription on the first iteration and cancels it when iteration ends, so
// callers never touch the underlying handle. A failure to subscribe surfaces as
// the first (and only) yielded error.
func (c *MoqOriginConsumer) Announcements(prefix string) iter.Seq2[*MoqAnnouncement, error] {
	return func(yield func(*MoqAnnouncement, error) bool) {
		announced, err := c.Announced(prefix)
		if err != nil {
			yield(nil, err)
			return
		}
		defer announced.Cancel()
		for {
			next, err := announced.Next()
			if err != nil {
				yield(nil, err)
				return
			}
			if next == nil {
				return
			}
			if !yield(*next, nil) {
				return
			}
		}
	}
}

// Groups streams a track's groups in sequence order, skipping ahead if the
// reader falls behind.
func (c *MoqTrackConsumer) Groups() iter.Seq2[*MoqGroupConsumer, error] {
	return func(yield func(*MoqGroupConsumer, error) bool) {
		for {
			next, err := c.NextGroup()
			if err != nil {
				yield(nil, err)
				return
			}
			if next == nil {
				return
			}
			if !yield(*next, nil) {
				return
			}
		}
	}
}

// GroupsAsArrived streams a track's groups in arrival order, including
// out-of-sequence deliveries.
func (c *MoqTrackConsumer) GroupsAsArrived() iter.Seq2[*MoqGroupConsumer, error] {
	return func(yield func(*MoqGroupConsumer, error) bool) {
		for {
			next, err := c.RecvGroup()
			if err != nil {
				yield(nil, err)
				return
			}
			if next == nil {
				return
			}
			if !yield(*next, nil) {
				return
			}
		}
	}
}

// Frames streams the raw frame payloads within a group.
func (c *MoqGroupConsumer) Frames() iter.Seq2[[]byte, error] {
	return func(yield func([]byte, error) bool) {
		for {
			next, err := c.ReadFrame()
			if err != nil {
				yield(nil, err)
				return
			}
			if next == nil {
				return
			}
			if !yield(*next, nil) {
				return
			}
		}
	}
}

// Frames streams decoded media frames in decode order.
func (c *MoqMediaConsumer) Frames() iter.Seq2[MoqFrame, error] {
	return func(yield func(MoqFrame, error) bool) {
		for {
			next, err := c.Next()
			if err != nil {
				yield(MoqFrame{}, err)
				return
			}
			if next == nil {
				return
			}
			if !yield(*next, nil) {
				return
			}
		}
	}
}

// Frames streams decoded audio frames in the layout declared by the
// MoqAudioDecoderConfig the consumer was created with.
func (c *MoqAudioConsumer) Frames() iter.Seq2[MoqAudioFrame, error] {
	return func(yield func(MoqAudioFrame, error) bool) {
		for {
			next, err := c.Next()
			if err != nil {
				yield(MoqAudioFrame{}, err)
				return
			}
			if next == nil {
				return
			}
			if !yield(*next, nil) {
				return
			}
		}
	}
}

// Updates streams catalog updates. Iteration ends when the underlying track ends.
func (c *MoqCatalogConsumer) Updates() iter.Seq2[MoqCatalog, error] {
	return func(yield func(MoqCatalog, error) bool) {
		for {
			next, err := c.Next()
			if err != nil {
				yield(MoqCatalog{}, err)
				return
			}
			if next == nil {
				return
			}
			if !yield(*next, nil) {
				return
			}
		}
	}
}
