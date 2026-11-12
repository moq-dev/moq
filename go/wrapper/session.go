package moq

import (
	"context"

	ffi "moq.dev/moq-ffi/moq"
	"moq.dev/moq/internal/bridge"
)

// Session is a MoQ connection, including its initial native dial. Hold it (or the Client/Server that
// produced it) to keep the connection alive; once every handle is dropped the
// connection closes.
type Session struct {
	inner *ffi.MoqSession
}

// Closed blocks until the session closes and returns the reason. A graceful
// close reports an error for which IsShutdown returns true. After Shutdown, it
// waits for the drain and reports delivery failures. Cancelling ctx gives
// up waiting and shuts the session down, so a caller that no longer cares about
// the connection can tear it down by cancelling.
func (s *Session) Closed(ctx context.Context) error {
	return bridge.CallErr(ctx, s.abort, s.inner.Closed)
}

// ConnectionState is one coherent lifecycle snapshot.
type ConnectionState struct {
	// Revision is the opaque token for the next StateChanged wait.
	Revision uint64
	// Connects counts successful sessions, including replacements.
	Connects uint64
	// Phase is where the connection is, carrying that phase's data.
	Phase ConnectionPhase
}

// ConnectionPhase is where a connection is in its lifecycle, carrying that phase's data:
// ConnectionPhaseConnecting, ConnectionPhaseConnected, ConnectionPhaseDisconnected,
// ConnectionPhaseMigrating, or ConnectionPhaseClosed.
type ConnectionPhase interface {
	isConnectionPhase()
}

// ConnectionPhaseConnecting means no session has connected yet.
type ConnectionPhaseConnecting struct {
	// Failure is the latest failed dial, or nil before the first one fails.
	Failure error
}

// ConnectionPhaseConnected means a session is serving.
type ConnectionPhaseConnected struct {
	// Version names the live session's negotiated protocol, like "moq-lite-03".
	Version string
}

// ConnectionPhaseDisconnected means no session is serving while the connection redials.
type ConnectionPhaseDisconnected struct {
	// Failure is why nothing serves: the last session's close reason or the latest failed dial.
	Failure error
}

// ConnectionPhaseMigrating means the peer sent a GOAWAY: the old session serves while its replacement dials.
type ConnectionPhaseMigrating struct {
	// Version names the serving session's negotiated protocol.
	Version string
	// Failure is the replacement's latest failed dial, or nil.
	Failure error
}

// ConnectionPhaseClosed means the connection has stopped for good.
type ConnectionPhaseClosed struct {
	// Failure is the terminal error, or nil after a local close.
	Failure error
}

func (ConnectionPhaseConnecting) isConnectionPhase()   {}
func (ConnectionPhaseConnected) isConnectionPhase()    {}
func (ConnectionPhaseDisconnected) isConnectionPhase() {}
func (ConnectionPhaseMigrating) isConnectionPhase()    {}
func (ConnectionPhaseClosed) isConnectionPhase()       {}

// failure keeps an absent error a nil error.
func failure(err **ffi.MoqError) error {
	if err == nil || *err == nil {
		return nil
	}
	return *err
}

func connectionStateFromFFI(state ffi.MoqConnectionState) ConnectionState {
	var phase ConnectionPhase
	switch p := state.Phase.(type) {
	case ffi.MoqConnectionPhaseConnecting:
		phase = ConnectionPhaseConnecting{Failure: failure(p.Failure)}
	case ffi.MoqConnectionPhaseConnected:
		phase = ConnectionPhaseConnected{Version: p.Version}
	case ffi.MoqConnectionPhaseDisconnected:
		phase = ConnectionPhaseDisconnected{Failure: failure(p.Failure)}
	case ffi.MoqConnectionPhaseMigrating:
		phase = ConnectionPhaseMigrating{Version: p.Version, Failure: failure(p.Failure)}
	case ffi.MoqConnectionPhaseClosed:
		phase = ConnectionPhaseClosed{Failure: failure(p.Failure)}
	}
	return ConnectionState{Revision: state.Revision, Connects: state.Connects, Phase: phase}
}

// State reads the lifecycle fields together without advancing a watcher.
func (s *Session) State() ConnectionState { return connectionStateFromFFI(s.inner.State()) }

// StateChanged waits for a revision different from after, without sharing a watcher cursor.
// Cancelling ctx only cancels this wait; it leaves the connection and other watchers running.
func (s *Session) StateChanged(ctx context.Context, after uint64) (ConnectionState, error) {
	state, err := bridge.Call(ctx, nil, func(ctx context.Context) (ffi.MoqConnectionState, error) {
		return s.inner.StateChanged(ctx, after)
	})
	return connectionStateFromFFI(state), err
}

// Stats snapshots the current connection statistics.
func (s *Session) Stats() ConnectionStats {
	return connectionStatsFromFFI(s.inner.Stats())
}

// Allocator is the session's bandwidth allocator. Every call returns a handle
// to the same registry, so reservations made through one are visible to the
// others. A client handle survives reconnects: the grant is nil while
// disconnected and resumes on the next connection.
func (s *Session) Allocator() *Allocator {
	return &Allocator{inner: s.inner.Allocator()}
}

// Grants follows the grant the peer gave this session. Each call returns an
// independent consumer; close it when done. A client session's grant follows
// each reconnect, reading nil while offline.
func (s *Session) Grants() *GrantConsumer {
	return &GrantConsumer{inner: s.inner.Grants()}
}

// GrantConsumer follows the grant a session's peer gave it: the union of every
// token the session presented.
//
// AUTH is only on moq-lite-07-wip; elsewhere the grant stays nil.
type GrantConsumer struct {
	inner *ffi.MoqGrantConsumer
}

// Grant returns the grant now, or nil until the peer first answers a token. A
// client session reads nil while it reconnects.
func (g *GrantConsumer) Grant() *Grant {
	return grantFromFFI(g.inner.Grant())
}

// Changed blocks until the grant differs from the one this last returned, or
// held when it was made, and returns it. It errors once a client's connection
// or a server's session ends, or after Cancel; a reconnect is not an end.
func (g *GrantConsumer) Changed(ctx context.Context) (*Grant, error) {
	grant, err := bridge.Call(ctx, nil, g.inner.Changed)
	return grantFromFFI(grant), err
}

// Cancel ends every current and future Changed call.
func (g *GrantConsumer) Cancel() { g.inner.Cancel() }

// Credential presents a request token on this session, for a [Route] or [Claim]
// to carry with SetAuth. On moq-transport it rides each naming route's
// announcing request; on moq-lite it needs AUTH, where it earns a grant. A
// client session presents it again on every reconnect, and with no session live
// it waits for the next one. An empty token errors with ErrConfig.
func (s *Session) Credential(token string) (*Credential, error) {
	inner, err := s.inner.Credential(token)
	if err != nil {
		return nil, err
	}
	return &Credential{inner: inner}, nil
}

// Credential is a request token a session presents. It stays presented until
// Cancel, or until the collector reclaims it and no route or claim names it,
// answered or not.
type Credential struct {
	inner *ffi.MoqCredential
}

// Answered blocks until the peer's first verdict on the token: nil once a peer
// accepts it, at once on moq-transport, where only the requests that name it
// carry the token. It returns the peer's refusal as a ProtocolError carrying
// its code, an error for which errors.Is(err, ErrUnsupported) holds when
// nothing can carry a token (moq-lite without AUTH), and ErrCancelled after
// Cancel.
func (c *Credential) Answered(ctx context.Context) error {
	return bridge.CallErr(ctx, nil, c.inner.Answered)
}

// Cancel releases the credential now: it is withdrawn at once unless a route or
// claim still names it, and every Answered call ends with ErrCancelled.
func (c *Credential) Cancel() { c.inner.Cancel() }

// ffi is the native credential a route or claim names, or none for nil.
func (c *Credential) ffi() **ffi.MoqCredential {
	if c == nil {
		return nil
	}
	return &c.inner
}

// Tokens returns the credentials a native client session presents, to replace
// while it runs, or nil for a server-accepted session, which presents none.
func (s *Session) Tokens() *Tokens {
	inner := s.inner.Tokens()
	if inner == nil {
		return nil
	}
	return &Tokens{inner: *inner}
}

// Tokens are the credentials a client session presents, replaceable while it runs.
type Tokens struct {
	inner *ffi.MoqTokens
}

// Set replaces the credentials every dial presents, such as before the current
// ones expire. The first is the connection credential, unless the URL carries
// its own ?jwt=; each other one widens the grant over in-band AUTH. The next
// dial presents them, and a live session with in-band AUTH presents them at
// once, withdrawing each replaced token once its replacements are answered,
// even when one is refused. After an authorization refusal the session redials
// only if they changed. An empty token is refused.
func (t *Tokens) Set(tokens ...string) error {
	return t.inner.Set(append([]string(nil), tokens...))
}

// Answered blocks until the peer's first verdict on the tokens the last Set
// newly presented: nil once each is granted, in band or as a later dial's
// connection credential. A token the set kept counts only once refused, and
// nothing is re-checked after the answer. It returns the peer's refusal as a
// ProtocolError carrying its code, an error for which
// errors.Is(err, ErrUnsupported) holds when the session cannot carry an
// in-band token, or the connection's close reason.
func (t *Tokens) Answered(ctx context.Context) error {
	return bridge.CallErr(ctx, nil, t.inner.Answered)
}

// Allocator divides one connection's send estimate among the tracks sharing it.
type Allocator struct {
	inner *ffi.MoqAllocator
}

// Reserve claims up to maxBps for the track demand watches; every track
// producer hands out its Demand. maxBps is a ceiling, not a measurement:
// reserve the most the track can ever send. Reuse Reservation.Update to change
// the ceiling, or call Update(0) before abandoning or replacing the reservation
// to hand the room back without waiting for garbage collection.
func (a *Allocator) Reserve(demand *TrackDemand, maxBps uint64) *Reservation {
	return &Reservation{inner: a.inner.Reserve(demand.inner, maxBps)}
}

// Reservation is one track's standing claim on an Allocator.
//
// Rate is a snapshot: nil means no estimate, so hold the current rate, and 0 is
// a real zero rate. While nothing subscribes it is the share the track would get.
// Update(0) relinquishes the allocation while the track stays subscribed; the
// handle remains usable and a later Update can claim bandwidth again.
type Reservation struct {
	inner *ffi.MoqReservation
}

// Rate returns this reservation's slice right now, in bits per second.
func (r *Reservation) Rate() *uint64 {
	return r.inner.Rate()
}

// Update changes the ceiling, keeping the same claim. Zero immediately gives
// its share to siblings and removes its contribution to the automatic probe
// target; it does not close the track or destroy the native handle.
func (r *Reservation) Update(maxBps uint64) {
	r.inner.Update(maxBps)
}

// Viewers returns the session's viewers, dividing the connection's receive
// estimate among the broadcasts watched over it. Every call returns a handle to
// the same registry, and the session sends what its viewers' ceilings add up to
// as its PROBE target. A client handle survives reconnects: a viewer reads nil
// while disconnected and resumes on the next connection.
func (s *Session) Viewers() *Viewers {
	return &Viewers{inner: s.inner.Viewers()}
}

// Viewers divides one connection's receive estimate among the broadcasts
// watched over it, max-min fair by each viewer's ceiling.
type Viewers struct {
	inner *ffi.MoqViewers
}

// Watch claims up to maxBps for a broadcast, returning its viewer. maxBps is a
// ceiling: what the largest rendition the player would switch to needs, not what
// it is receiving. Holding the viewer requests the receive estimate, like
// Session.RecvBandwidth; Close it once the broadcast stops being watched.
// Errors with ErrOutOfRange, watching nothing, when the ceilings would sum
// above 2^53 - 1.
func (v *Viewers) Watch(maxBps uint64) (*Viewer, error) {
	inner, err := v.inner.Watch(maxBps)
	if err != nil {
		return nil, err
	}
	return &Viewer{inner: inner}, nil
}

// SetTarget sends targetBps as the PROBE target instead of the viewers' sum, or
// nil to follow them again. Zero sends no target. Errors with ErrOutOfRange,
// keeping the previous override, above 2^53 - 1.
func (v *Viewers) SetTarget(targetBps *uint64) error {
	return v.inner.SetTarget(targetBps)
}

// Viewer is one watched broadcast's standing claim on Viewers.
//
// Rate is a snapshot: nil means no receive estimate, so hold the current
// selection, and 0 is a real zero rate.
type Viewer struct {
	inner *ffi.MoqViewer
}

// Rate returns this viewer's slice of the receive estimate right now, in bits
// per second.
func (v *Viewer) Rate() *uint64 {
	return v.inner.Rate()
}

// Update changes the ceiling, keeping the same claim, as when a catalog enables
// or disables a rendition. Errors with ErrOutOfRange, keeping the previous
// ceiling, when the ceilings would sum above 2^53 - 1.
func (v *Viewer) Update(maxBps uint64) error {
	return v.inner.Update(maxBps)
}

// Close releases the claim, handing its share to the other viewers, lowering
// the PROBE target, and ending its receive-estimate demand without waiting for
// garbage collection. Call it from the goroutine that last used the viewer;
// the viewer is unusable afterwards.
func (v *Viewer) Close() {
	v.inner.Destroy()
}

// RatePolicy maps a reservation's grant onto a sender's bitrate: MaxBps caps it,
// and LowerBps, the next lower rendition's MaxBps, disables this rendition while
// the grant is below (MaxBps + 2*LowerBps) / 3 and a session probes for it.
type RatePolicy = ffi.MoqRatePolicy

// RateTarget is what a sender following a RateControl should be doing: a
// RateTargetEnabled bitrate, or RateTargetDisabled.
type RateTarget = ffi.MoqRateTarget

// RateTargetEnabled produces at BitrateBps and advertises the rendition as enabled.
type RateTargetEnabled = ffi.MoqRateTargetEnabled

// RateTargetDisabled stops sending and advertises the rendition as disabled.
type RateTargetDisabled = ffi.MoqRateTargetDisabled

// RateControl turns a Reservation's grant into a sender's RateTarget, with the
// policy moq's own encoders follow: drops apply at once, raises climb at most 25%
// per second, and moves under 5% are ignored. For a sender that encodes on its
// own, call Update on every frame and apply what it returns.
type RateControl struct {
	inner *ffi.MoqRateControl
}

// NewRateControl starts at policy.MaxBps until a grant says otherwise.
func NewRateControl(policy RatePolicy) *RateControl {
	return &RateControl{inner: ffi.NewMoqRateControl(policy)}
}

// Target returns what the sender should currently be doing.
func (c *RateControl) Target() RateTarget {
	return c.inner.Target()
}

// Update feeds reservation's grant, returning a new target worth applying, or nil.
func (c *RateControl) Update(reservation *Reservation) *RateTarget {
	return c.inner.Update(reservation.inner)
}

// Publish returns the origin used to advertise local broadcasts to the remote.
func (s *Session) Publish() *OriginProducer {
	return &OriginProducer{inner: s.inner.Publish()}
}

// Consume returns the origin used to receive broadcasts from the remote.
func (s *Session) Consume() *OriginConsumer {
	return &OriginConsumer{inner: s.inner.Consume()}
}

// Shutdown requests a draining close now; wait for its result with Closed.
// moq-lite-05 and newer wait for the peer to receive finished tails; older Lite
// versions and IETF wait only for transport ACKs.
// Finish or abort live tracks first; the drain times out after one second.
func (s *Session) Shutdown() {
	s.inner.Shutdown()
}

// abort tears the session down without draining, for a cancelled ctx.
func (s *Session) abort() {
	s.inner.Cancel(0)
}

// Cancel closes the session abruptly with an application error code.
func (s *Session) Cancel(code uint32) {
	s.inner.Cancel(code)
}

// RecvBandwidth holds receive-estimate demand until its consumer is cancelled.
func (s *Session) RecvBandwidth() *BandwidthConsumer {
	inner := s.inner.RecvBandwidth()
	if inner == nil {
		return nil
	}
	return &BandwidthConsumer{inner: *inner}
}

// BandwidthConsumer owns a bandwidth estimate, in bits per second.
type BandwidthConsumer struct{ inner *ffi.MoqBandwidthConsumer }

// Rate is the current estimate, or nil while unavailable.
func (b *BandwidthConsumer) Rate() *uint64 { return b.inner.Rate() }

// RateChanged waits for the estimate to change and returns the new Rate, or fails once its producer closes.
func (b *BandwidthConsumer) RateChanged(ctx context.Context) (*uint64, error) {
	return bridge.Call(ctx, nil, b.inner.RateChanged)
}

// Cancel releases this consumer's demand, including a pending wait.
func (b *BandwidthConsumer) Cancel() { b.inner.Cancel() }
