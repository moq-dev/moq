package moq

import (
	"context"

	ffi "github.com/moq-dev/moq-go-ffi/moq"
)

// Session is an established MoQ connection. Hold it (or the Client/Server that
// produced it) to keep the connection alive; once every handle is dropped the
// connection closes.
type Session struct {
	inner *ffi.MoqSession
}

// Closed blocks until the session closes and returns the reason. A graceful
// close reports an error for which IsShutdown returns true. Cancelling ctx gives
// up waiting and shuts the session down, so a caller that no longer cares about
// the connection can tear it down by cancelling.
func (s *Session) Closed(ctx context.Context) error {
	return runErr(ctx, s.inner.Shutdown, s.inner.Closed)
}

// Stats snapshots the current connection statistics.
func (s *Session) Stats() ConnectionStats {
	return s.inner.Stats()
}

// Publisher returns the origin used to advertise local broadcasts to the remote.
func (s *Session) Publisher() *OriginProducer {
	return &OriginProducer{inner: s.inner.Publisher()}
}

// Consumer returns the origin used to receive broadcasts from the remote.
func (s *Session) Consumer() *OriginConsumer {
	return &OriginConsumer{inner: s.inner.Consumer()}
}

// Shutdown closes the session gracefully.
func (s *Session) Shutdown() {
	s.inner.Shutdown()
}

// Cancel closes the session abruptly with an application error code.
func (s *Session) Cancel(code uint32) {
	s.inner.Cancel(code)
}
