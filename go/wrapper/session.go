package moq

import (
	"context"

	ffi "moq.dev/moq-ffi/moq"
	"moq.dev/moq/internal/bridge"
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
	return bridge.CallErr(ctx, s.abort, s.inner.Closed)
}

// Status blocks until the connection status differs from the one this session
// last reported. A client session reports ConnectionStatusConnected first (the connect it
// was built from), then follows the reconnect loop: ConnectionStatusDisconnected while
// redialing, ConnectionStatusMigrating during a GOAWAY handover. It returns an error once
// the connection stops for good. A server-accepted session's only transition is
// terminal, so Status waits for the close and returns its reason. Cancelling ctx
// gives up waiting and shuts the session down.
//
// This reports the current status rather than every edge: a drop that reconnects
// before the next call is coalesced away, so the outages it hides are the ones
// that already healed. Don't count outages with it.
func (s *Session) Status(ctx context.Context) (ConnectionStatus, error) {
	return bridge.Call(ctx, s.abort, s.inner.Status)
}

// Connects counts how many times this session has connected: 1 for the connect that built it, one
// more on each reconnect. A server-accepted session stays at 1.
//
// Pair it with Status to log each reconnect by number: a ConnectionStatusConnected whose
// count grew is a reconnect. Like Status, it reports the current state, so a
// drop that reconnects between reads is coalesced away.
func (s *Session) Connects() uint64 {
	return s.inner.Connects()
}

// Failure reports why a client session is reconnecting: the error that ended
// the last connection or failed the latest redial, or nil once a connection
// lands. A redial that keeps failing never changes Status, so poll this while it
// reports ConnectionStatusDisconnected to show the reason. Giving up for good
// returns from Closed instead. A server-accepted session always returns nil.
func (s *Session) Failure() error {
	failure := s.inner.Failure()
	if failure == nil || *failure == nil {
		return nil
	}
	return *failure
}

// Stats snapshots the current connection statistics.
func (s *Session) Stats() ConnectionStats {
	return connectionStatsFromFFI(s.inner.Stats())
}

// Bandwidth is the session's bandwidth allocator. Every call returns a handle
// to the same registry, so reservations made through one are visible to the
// others. A client handle survives reconnects: the grant is nil while
// disconnected and resumes on the next connection.
func (s *Session) Bandwidth() *Bandwidth {
	return &Bandwidth{inner: s.inner.Bandwidth()}
}

// Auth is the session's authorization: the grant its peer gave it, and more
// tokens to present. Each call returns an independent watcher of the grant. A
// client session's handle survives reconnects, presenting each added token again
// on every new connection.
func (s *Session) Auth() *Auth {
	return &Auth{inner: s.inner.Auth()}
}

// Auth reads a session's grant and presents more tokens.
//
// AUTH is only on moq-lite-07-wip and on moq-transport sessions that negotiate
// the MoQ Auth extension; elsewhere the grant stays nil.
type Auth struct {
	inner *ffi.MoqAuth
}

// Grant returns the union of every token's grant, or nil until the peer first
// answers one. A client session reads nil while it reconnects.
func (a *Auth) Grant() *Grant {
	return grantFromFFI(a.inner.Grant())
}

// GrantChanged blocks until the grant differs from the one this handle last
// reported, or held when it was made, and returns it. It errors once the session
// ends.
func (a *Auth) GrantChanged(ctx context.Context) (*Grant, error) {
	grant, err := bridge.Call(ctx, nil, a.inner.GrantChanged)
	return grantFromFFI(grant), err
}

// Add presents another token, returning once the peer accepts it. Its grant
// joins the union until the token is closed. The peer's refusal is a
// ProtocolError carrying its code; a session that cannot carry another token
// returns an error for which errors.Is(err, ErrUnsupported) holds. Cancelling ctx
// withdraws the token.
func (a *Auth) Add(ctx context.Context, token string) (*AuthToken, error) {
	inner, err := bridge.CallHandle(ctx, nil, func(ctx context.Context) (*ffi.MoqAuthToken, error) {
		return a.inner.Add(ctx, token)
	})
	if err != nil {
		return nil, err
	}
	return &AuthToken{inner: inner}, nil
}

// AuthToken is one token presented with Auth.Add.
type AuthToken struct {
	inner *ffi.MoqAuthToken
}

// Grant returns this token's own grant, or nil after it ended.
func (t *AuthToken) Grant() *Grant {
	return grantFromFFI(t.inner.Grant())
}

// Closed blocks until the token ends, returning the peer's revocation or the
// session's close, or nil when the peer closed it without an error.
func (t *AuthToken) Closed(ctx context.Context) error {
	return bridge.CallErr(ctx, nil, t.inner.Closed)
}

// Close withdraws the token, removing its grant from the union.
func (t *AuthToken) Close() { t.inner.Cancel() }

// Bandwidth divides one connection's send estimate among the tracks sharing it.
type Bandwidth struct {
	inner *ffi.MoqBandwidth
}

// Reserve claims up to maxBps for the track demand watches; every track
// producer hands out its Demand. maxBps is a ceiling, not a measurement:
// reserve the most the track can ever send. Reuse Reservation.Update to change
// the ceiling, or call Update(0) before abandoning or replacing the reservation
// to hand the room back without waiting for garbage collection.
func (b *Bandwidth) Reserve(demand *TrackDemand, maxBps uint64) *Reservation {
	return &Reservation{inner: b.inner.Reserve(demand.inner, maxBps)}
}

// SetTarget probes up to targetBps instead of the sum of the reservations. The
// session probes for what its reservations add up to by default, so the
// estimate can rise to fund them. 0 turns probing off, and nil follows the
// reservations again.
func (b *Bandwidth) SetTarget(targetBps *uint64) {
	b.inner.SetTarget(targetBps)
}

// Reservation is one track's standing claim on a Bandwidth.
//
// Grant is a snapshot: nil means no estimate or no demand, so hold the current
// rate, and 0 is a real zero grant.
// Update(0) relinquishes the allocation while the track stays subscribed; the
// handle remains usable and a later Update can claim bandwidth again.
type Reservation struct {
	inner *ffi.MoqReservation
}

// Grant returns this reservation's slice right now, in bits per second.
func (r *Reservation) Grant() *uint64 {
	return r.inner.Grant()
}

// Update changes the ceiling, keeping the same claim. Zero immediately gives
// its share to siblings and removes its contribution to the automatic probe
// target; it does not close the track or destroy the native handle.
func (r *Reservation) Update(maxBps uint64) {
	r.inner.Update(maxBps)
}

// Publish returns the origin used to advertise local broadcasts to the remote.
func (s *Session) Publish() *OriginProducer {
	return &OriginProducer{inner: s.inner.Publish()}
}

// Consume returns the origin used to receive broadcasts from the remote.
func (s *Session) Consume() *OriginConsumer {
	return &OriginConsumer{inner: s.inner.Consume()}
}

// Shutdown drains finished tracks within one second, returning any drain error.
// Explicit moq-lite-07-wip waits for the peer to receive finished tails; older Lite
// versions, including the default lite-06, wait only for transport ACKs.
// IETF also waits for transport ACKs.
// Cancelling ctx aborts the session immediately.
func (s *Session) Shutdown(ctx context.Context) error {
	return bridge.CallErr(ctx, s.abort, s.inner.Shutdown)
}

// abort tears the session down without draining, for a cancelled ctx.
func (s *Session) abort() {
	s.inner.Cancel(0)
}

// Cancel closes the session abruptly with an application error code.
func (s *Session) Cancel(code uint32) {
	s.inner.Cancel(code)
}

// RecvBandwidth holds receive-estimate demand until its consumer is closed.
func (s *Session) RecvBandwidth() *BandwidthConsumer {
	inner := s.inner.RecvBandwidth()
	if inner == nil {
		return nil
	}
	return &BandwidthConsumer{inner: *inner}
}

// BandwidthConsumer owns a bandwidth estimate, in bits per second.
type BandwidthConsumer struct{ inner *ffi.MoqBandwidthConsumer }

// Peek reads the current estimate, or nil while unavailable.
func (b *BandwidthConsumer) Peek() *uint64 { return b.inner.Peek() }

// Changed waits for the estimate to change, or its producer to close.
func (b *BandwidthConsumer) Changed(ctx context.Context) (*uint64, error) {
	return bridge.Call(ctx, b.inner.Cancel, b.inner.Changed)
}

// Close releases this consumer's demand, including a pending wait.
func (b *BandwidthConsumer) Close() { b.inner.Cancel() }
