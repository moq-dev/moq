package moq

import (
	"context"
	"iter"
	"sync"

	ffi "moq.dev/moq-ffi/moq"
	"moq.dev/moq/internal/bridge"
)

// Transport is the network transport carrying an incoming session.
type Transport = ffi.MoqTransport

const (
	// TransportQUIC is a session that arrived over native QUIC.
	TransportQUIC = ffi.MoqTransportQuic
	// TransportIroh is a session that arrived over an Iroh peer-to-peer connection.
	TransportIroh = ffi.MoqTransportIroh
	// TransportWebSocket is a session that arrived over the WebSocket fallback transport.
	TransportWebSocket = ffi.MoqTransportWebSocket
	// TransportTCP is a session that arrived over a plaintext TCP connection.
	TransportTCP = ffi.MoqTransportTcp
	// TransportUnix is a session that arrived over a Unix domain socket.
	TransportUnix = ffi.MoqTransportUnix
)

// Request is an incoming session that can be accepted (Accept) or rejected (Reject).
// Dropping a Request without responding closes the connection silently.
type Request struct {
	inner *ffi.MoqRequest
}

// URL is the requested URL, if any.
func (r *Request) URL() *string {
	return r.inner.Url()
}

// Path is the query-free request path, or an empty string for the root/missing path.
func (r *Request) Path() string {
	return r.inner.Path()
}

// Query is the encoded request query without "?"; it may contain credentials.
func (r *Request) Query() *string {
	return r.inner.Query()
}

// Transport is the wire transport the request arrived over, e.g. TransportQUIC.
func (r *Request) Transport() Transport {
	return r.inner.Transport()
}

// Accept completes the handshake, inheriting server origins wherever an argument is nil.
// Hold the session to keep the connection alive. Pass fresh origins for isolation,
// or the same fresh origin on both sides to share it.
func (r *Request) Accept(ctx context.Context, publish, consume *OriginProducer) (*Session, error) {
	var ffiPublish, ffiConsume **ffi.MoqOriginProducer
	if publish != nil {
		ffiPublish = &publish.inner
	}
	if consume != nil {
		ffiConsume = &consume.inner
	}
	inner, err := bridge.CallHandle(ctx, r.inner.Cancel, func(ctx context.Context) (*ffi.MoqSession, error) {
		return r.inner.Accept(ctx, ffiPublish, ffiConsume)
	})
	if err != nil {
		return nil, err
	}
	return &Session{inner: inner}, nil
}

// Reject refuses the session with an application error code; 401 and 403 map to unauthorized.
func (r *Request) Reject(ctx context.Context, code uint16) error {
	return bridge.CallErr(ctx, r.inner.Cancel, func(ctx context.Context) error {
		return r.inner.Reject(ctx, code)
	})
}

// Cancel aborts an in-flight Accept or Reject.
func (r *Request) Cancel() {
	r.inner.Cancel()
}

// ServerOption configures a server created with Listen.
type ServerOption func(*serverConfig)

type serverConfig struct {
	tlsCert        []string
	tlsKey         []string
	tlsGenerate    []string
	versions       []string
	quicMaxStreams *uint64
	publish        *OriginProducer
	consume        *OriginProducer
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

// WithServerVersions restricts the protocol versions accepted, spelled like
// "moq-lite-03". By default every supported version is accepted.
func WithServerVersions(versions ...string) ServerOption {
	accepted := append([]string(nil), versions...)
	return func(c *serverConfig) { c.versions = accepted }
}

// WithServerQUICMaxStreams caps the concurrent QUIC streams each peer may open
// toward this server (default 1024).
func WithServerQUICMaxStreams(maxStreams uint64) ServerOption {
	return func(c *serverConfig) { c.quicMaxStreams = &maxStreams }
}

// WithServerPublishOrigin sets the origin whose broadcasts are served to
// incoming sessions. Omit both origin options to get a shared internal origin.
func WithServerPublishOrigin(o *OriginProducer) ServerOption {
	return func(c *serverConfig) { c.publish = o }
}

// WithServerConsumeOrigin sets the origin that receives broadcasts published
// by incoming sessions.
func WithServerConsumeOrigin(o *OriginProducer) ServerOption {
	return func(c *serverConfig) { c.consume = o }
}

// Server accepts incoming sessions with automatic origin wiring.
type Server struct {
	inner         *ffi.MoqServer
	publishOrigin *OriginProducer
	localAddr     string
	closeOnce     sync.Once
}

// Listen binds the server and starts accepting. Cancel ctx to abort the bind.
func Listen(ctx context.Context, bind string, opts ...ServerOption) (*Server, error) {
	var cfg serverConfig
	for _, opt := range opts {
		opt(&cfg)
	}
	if cfg.publish == nil && cfg.consume == nil {
		shared := NewOriginProducer()
		cfg.publish = shared
		cfg.consume = shared
	}

	config := ffi.MoqServerConfig{
		Bind:     &bind,
		Versions: cfg.versions,
		Tls: ffi.MoqServerTls{
			Cert:     cfg.tlsCert,
			Key:      cfg.tlsKey,
			Generate: cfg.tlsGenerate,
		},
		Quic: ffi.MoqQuicConfig{MaxStreams: cfg.quicMaxStreams},
	}
	if cfg.publish != nil {
		config.Publish = &cfg.publish.inner
	}
	if cfg.consume != nil {
		config.Consume = &cfg.consume.inner
	}

	inner, err := ffi.NewMoqServer(config)
	if err != nil {
		return nil, err
	}
	s := &Server{inner: inner, publishOrigin: cfg.publish}

	addr, err := bridge.Call(ctx, inner.Cancel, inner.Listen)
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

// CreateBroadcast creates an unannounced broadcast at path, invisible to everyone until announced. Announce it after populating tracks.
//
// See [OriginProducer.CreateBroadcast].
func (s *Server) CreateBroadcast(path string) (*BroadcastProducer, error) {
	if s.publishOrigin == nil {
		return nil, ErrNoPublishOrigin
	}
	return s.publishOrigin.CreateBroadcast(path)
}

// Accept returns the next incoming request, or (nil, nil) when the server stops.
//
// Cancelling ctx aborts this accept alone and leaves the server listening; use
// Close to tear the listener down.
func (s *Server) Accept(ctx context.Context) (*Request, error) {
	res, err := s.inner.Accept(ctx)
	if err != nil || res == nil {
		return nil, err
	}
	return &Request{inner: *res}, nil
}

// All ranges over incoming requests until the server stops or the loop
// breaks. Each request must be answered with Accept or Reject.
func (s *Server) All(ctx context.Context) iter.Seq2[*Request, error] {
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

// Serve accepts every session in a loop, holding each one alive in its own
// goroutine until it closes, so memory does not grow with past connections. It
// returns when Accept stops (nil) or fails, after the in-flight sessions wind
// down.
//
// To inspect or reject requests, range over Requests instead:
//
//	for req, err := range server.All(ctx) {
//	    if err != nil {
//	        return err
//	    }
//	    if req.Path() == "/admin" {
//	        _ = req.Reject(ctx, 403)
//	        continue
//	    }
//	    session, err := req.Accept(ctx, nil, nil)
//	    // hold the session to keep the connection alive
//	}
func (s *Server) Serve(ctx context.Context) error {
	var wg sync.WaitGroup
	defer wg.Wait()

	for {
		req, err := s.Accept(ctx)
		if err != nil {
			// ctx-driven shutdown: the accept is already aborted, so this is what
			// stops the listener itself.
			if ctx.Err() != nil {
				_ = s.Close()
			}
			return err
		}
		if req == nil {
			return nil
		}

		wg.Add(1)
		go func(req *Request) {
			defer wg.Done()
			session, err := req.Accept(ctx, nil, nil)
			if err != nil {
				return
			}
			// Hold the session until it closes; Closed shuts it down if ctx is
			// cancelled so this goroutine (and the deferred Wait) can't hang.
			_ = session.Closed(ctx)
		}(req)
	}
}

// Close stops accepting new sessions and releases the listening socket before
// it returns, so the same address can be bound again immediately. In-flight
// sessions stay alive until their handles are dropped or cancelled. Safe to call
// more than once and from multiple goroutines (Serve calls it on ctx-driven
// shutdown).
func (s *Server) Close() error {
	s.closeOnce.Do(func() {
		if s.inner != nil {
			s.inner.Cancel()
		}
	})
	return nil
}
