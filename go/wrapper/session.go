package moq

import (
	"context"

	ffi "moq.dev/moq-ffi/moq"
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

// Epoch is the connection epoch: 1 for the connect that built this session, one
// more on each reconnect. A server-accepted session stays at 1.
//
// Pair it with Status to log each reconnect by number: a StatusConnected whose
// Epoch grew is a reconnect. Like Status, it reports the current state, so a
// drop that reconnects between reads is coalesced away.
func (s *Session) Epoch() uint64 {
	return s.inner.Epoch()
}

// Stats snapshots the current connection statistics.
func (s *Session) Stats() ConnectionStats {
	return s.inner.Stats()
}

// Bandwidth is the session's bandwidth allocator. Every call returns a handle
// to the same registry, so reservations made through one are visible to the
// others. A client handle survives reconnects: the grant is nil while
// disconnected and resumes on the next connection.
func (s *Session) Bandwidth() *Bandwidth {
	return &Bandwidth{inner: s.inner.Bandwidth()}
}

// Bandwidth divides one connection's send estimate among the tracks sharing it.
type Bandwidth struct {
	inner *ffi.MoqBandwidth
}

// Reserve claims up to maxBps for track. maxBps is a ceiling, not a
// measurement: reserve the most the track can ever send. Drop the reservation
// to hand the room back.
func (b *Bandwidth) Reserve(track *TrackProducer, maxBps uint64) (*Reservation, error) {
	inner, err := b.inner.Reserve(track.inner, maxBps)
	if err != nil {
		return nil, err
	}
	return &Reservation{inner: inner}, nil
}

// Reservation is one track's standing claim on a Bandwidth.
//
// Grant is a snapshot: nil means no estimate or no demand, so hold the current
// rate, and 0 is a real zero grant.
type Reservation struct {
	inner *ffi.MoqReservation
}

// Grant returns this reservation's slice right now, in bits per second.
func (r *Reservation) Grant() *uint64 {
	return r.inner.Grant()
}

// Update changes the ceiling, keeping the same claim.
func (r *Reservation) Update(maxBps uint64) {
	r.inner.Update(maxBps)
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
