package moq

import (
	"context"
	"iter"
	"sync"

	ffi "github.com/moq-dev/moq-go-ffi/moq"
)

// Transport is the wire transport an incoming session arrived over.
type Transport string

// Known transports reported by Request.Transport. Future native versions may
// report values not listed here, so treat Transport as an open set.
const (
	TransportQUIC      Transport = "quic"
	TransportIroh      Transport = "iroh"
	TransportWebSocket Transport = "websocket"
)

// Request is an incoming session that can be accepted (OK) or rejected (Close).
// Dropping a Request without responding closes the connection silently.
type Request struct {
	inner *ffi.MoqRequest
}

// URL is the requested URL, if any.
func (r *Request) URL() *string {
	return r.inner.Url()
}

// Transport is the wire transport the request arrived over, e.g. TransportQUIC.
func (r *Request) Transport() Transport {
	return Transport(r.inner.Transport())
}

// SetPublish overrides the publish origin for this session. Pass nil to fall
// back to the server's configured publish origin.
func (r *Request) SetPublish(o *OriginProducer) {
	if o == nil {
		r.inner.SetPublish(nil)
		return
	}
	r.inner.SetPublish(&o.inner)
}

// SetConsume overrides the consume origin for this session. Pass nil to fall
// back to the server's configured consume origin.
func (r *Request) SetConsume(o *OriginProducer) {
	if o == nil {
		r.inner.SetConsume(nil)
		return
	}
	r.inner.SetConsume(&o.inner)
}

// OK completes the handshake and returns the established session. Hold the
// session to keep the connection alive.
func (r *Request) OK(ctx context.Context) (*Session, error) {
	inner, err := runCancellable(ctx, r.inner.Cancel, r.inner.Ok)
	if err != nil {
		return nil, err
	}
	return &Session{inner: inner}, nil
}

// Close rejects the session with an HTTP status code (default convention: 404).
func (r *Request) Close(ctx context.Context, code uint16) error {
	return runErr(ctx, r.inner.Cancel, func() error {
		return r.inner.Close(code)
	})
}

// Cancel aborts an in-flight OK or Close.
func (r *Request) Cancel() {
	r.inner.Cancel()
}

// ServerOption configures a server created with Listen.
type ServerOption func(*serverConfig)

type serverConfig struct {
	tlsCert     []string
	tlsKey      []string
	tlsGenerate []string
	publish     *OriginProducer
	subscribe   *OriginProducer
}

// WithTLSCert sets paths to TLS certificate chains.
func WithTLSCert(paths ...string) ServerOption {
	return func(c *serverConfig) { c.tlsCert = paths }
}

// WithTLSKey sets paths to TLS private keys.
func WithTLSKey(paths ...string) ServerOption {
	return func(c *serverConfig) { c.tlsKey = paths }
}

// WithTLSGenerate generates a self-signed certificate for the given hostnames.
func WithTLSGenerate(hostnames ...string) ServerOption {
	return func(c *serverConfig) { c.tlsGenerate = hostnames }
}

// WithServerPublishOrigin sets the origin whose broadcasts are served to
// incoming sessions. Omit both origin options to get a shared internal origin.
func WithServerPublishOrigin(o *OriginProducer) ServerOption {
	return func(c *serverConfig) { c.publish = o }
}

// WithServerSubscribeOrigin sets the origin that receives broadcasts published
// by incoming sessions.
func WithServerSubscribeOrigin(o *OriginProducer) ServerOption {
	return func(c *serverConfig) { c.subscribe = o }
}

// Server accepts incoming sessions with automatic origin wiring.
type Server struct {
	inner         *ffi.MoqServer
	origin        *OriginProducer
	publishOrigin *OriginProducer
	consumeOrigin *OriginProducer
	localAddr     string
	closeOnce     sync.Once
}

// Listen binds the server and starts accepting. Cancel ctx to abort the bind.
func Listen(ctx context.Context, bind string, opts ...ServerOption) (*Server, error) {
	var cfg serverConfig
	for _, opt := range opts {
		opt(&cfg)
	}

	s := &Server{}
	if cfg.publish == nil && cfg.subscribe == nil {
		s.origin = NewOriginProducer()
		s.publishOrigin = s.origin
		s.consumeOrigin = s.origin
	} else {
		s.publishOrigin = cfg.publish
		s.consumeOrigin = cfg.subscribe
	}

	inner := ffi.NewMoqServer()
	if err := inner.SetBind(bind); err != nil {
		inner.Cancel()
		return nil, err
	}
	if len(cfg.tlsCert) > 0 {
		inner.SetTlsCert(cfg.tlsCert)
	}
	if len(cfg.tlsKey) > 0 {
		inner.SetTlsKey(cfg.tlsKey)
	}
	if len(cfg.tlsGenerate) > 0 {
		inner.SetTlsGenerate(cfg.tlsGenerate)
	}
	if s.publishOrigin != nil {
		inner.SetPublish(&s.publishOrigin.inner)
	}
	if s.consumeOrigin != nil {
		inner.SetConsume(&s.consumeOrigin.inner)
	}
	s.inner = inner

	addr, err := runCancellable(ctx, inner.Cancel, inner.Listen)
	if err != nil {
		inner.Cancel()
		return nil, err
	}
	s.localAddr = addr
	return s, nil
}

// LocalAddr is the bound local address.
func (s *Server) LocalAddr() string {
	return s.localAddr
}

// CertFingerprints returns the SHA-256 fingerprints of the configured TLS
// certificates, hex-encoded. Useful for pinning a generated self-signed
// certificate in a browser via WebTransport's serverCertificateHashes.
func (s *Server) CertFingerprints() ([]string, error) {
	return s.inner.CertFingerprints()
}

// Publish publishes a broadcast under path, served to incoming sessions.
func (s *Server) Publish(path string, broadcast *BroadcastProducer) error {
	if s.publishOrigin == nil {
		return ErrNoPublishOrigin
	}
	return s.publishOrigin.Publish(path, broadcast)
}

// Accept returns the next incoming request, or (nil, nil) when the server stops.
//
// The ffi listener has no per-accept cancellation (its only cancel is the
// server-wide one Close uses). Canceling ctx therefore makes Accept return
// ctx.Err() while the underlying accept keeps running in the background until a
// connection arrives or Close stops the server. Use Close to tear the listener
// down; don't rely on a per-call ctx to do it. Serve handles this for you.
func (s *Server) Accept(ctx context.Context) (*Request, error) {
	res, err := runCancellable(ctx, nil, s.inner.Accept)
	if err != nil {
		return nil, err
	}
	if res == nil {
		return nil, nil
	}
	return &Request{inner: *res}, nil
}

// Requests ranges over incoming requests until the server stops or the loop
// breaks. Each request must be answered with OK or Close.
func (s *Server) Requests(ctx context.Context) iter.Seq2[*Request, error] {
	return func(yield func(*Request, error) bool) {
		for {
			req, err := s.Accept(ctx)
			if err != nil {
				yield(nil, err)
				return
			}
			if req == nil {
				return
			}
			if !yield(req, nil) {
				return
			}
		}
	}
}

// Serve accepts sessions in a loop, holding each one alive in its own goroutine
// until it closes, so memory does not grow with past connections. It returns
// when Accept stops (nil) or fails, after the in-flight sessions wind down.
//
// Pass onRequest to inspect a Request before accepting: return false to reject
// with HTTP 403, or an error to reject with 500 and stop serving. Pass nil to
// accept every request. For richer routing, drive Requests/Accept directly.
func (s *Server) Serve(ctx context.Context, onRequest func(*Request) (bool, error)) error {
	var wg sync.WaitGroup
	defer wg.Wait()

	for {
		req, err := s.Accept(ctx)
		if err != nil {
			// ctx-driven shutdown: Close stops the listener and unblocks the
			// background accept (which has no per-call cancel of its own).
			if ctx.Err() != nil {
				_ = s.Close()
			}
			return err
		}
		if req == nil {
			return nil
		}

		if onRequest != nil {
			ok, err := onRequest(req)
			if err != nil {
				_ = req.Close(ctx, 500)
				return err
			}
			if !ok {
				_ = req.Close(ctx, 403)
				continue
			}
		}

		wg.Add(1)
		go func(req *Request) {
			defer wg.Done()
			session, err := req.OK(ctx)
			if err != nil {
				return
			}
			// Hold the session until it closes; Closed shuts it down if ctx is
			// cancelled so this goroutine (and the deferred Wait) can't hang.
			_ = session.Closed(ctx)
		}(req)
	}
}

// Close stops accepting new sessions. In-flight sessions stay alive until their
// handles are dropped or cancelled. Safe to call more than once and from
// multiple goroutines (Serve calls it on ctx-driven shutdown).
func (s *Server) Close() error {
	s.closeOnce.Do(func() {
		if s.inner != nil {
			s.inner.Cancel()
		}
	})
	return nil
}
