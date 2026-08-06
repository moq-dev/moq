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

// Status blocks until the connection status differs from the one this session
// last reported. A client session reports StatusConnected first (the connect it
// was built from), then follows the reconnect loop: StatusDisconnected while
// redialing, StatusMigrating during a GOAWAY handover. It returns an error once
// the connection stops for good. A server-accepted session's only transition is
// terminal, so Status waits for the close and returns its reason. Cancelling ctx
// gives up waiting and shuts the session down.
//
// This reports the current status rather than every edge: a drop that reconnects
// before the next call is coalesced away, so the outages it hides are the ones
// that already healed. Don't count outages with it.
func (s *Session) Status(ctx context.Context) (ConnectionStatus, error) {
	return runCancellable(ctx, s.inner.Shutdown, s.inner.Status)
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
