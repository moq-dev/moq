package moq

import (
	"context"
	"iter"
)

// streamSeq turns a pointer-returning, end-on-nil Next method into a Go 1.23
// range-over-func sequence. It yields (value, nil) for each item, yields
// (nil, err) once if a call fails, and stops cleanly when Next returns nil
// (the stream ended) or when the consumer breaks out of the range loop.
//
//	for frame, err := range consumer.Frames(ctx) {
//	    if err != nil {
//	        if moq.IsShutdown(err) { break }
//	        return err
//	    }
//	    // use frame
//	}
func streamSeq[T any](ctx context.Context, next func(context.Context) (*T, error)) iter.Seq2[*T, error] {
	return func(yield func(*T, error) bool) {
		for {
			val, err := next(ctx)
			if err != nil {
				yield(nil, err)
				return
			}
			if val == nil {
				return
			}
			if !yield(val, nil) {
				return
			}
		}
	}
}
