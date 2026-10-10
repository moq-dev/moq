package moq

import (
	"context"
	"sync"
	"time"

	ffi "moq.dev/moq-ffi/moq"
	"moq.dev/moq/internal/bridge"
)

// ClientOption configures a client created with Dial.
type ClientOption func(*clientConfig)

// clientConfig collects the options; Dial validates it into the native config,
// whose unset fields keep moq-ffi's defaults.
type clientConfig struct {
	tlsInsecure      bool
	tlsRoots         []string
	tlsSystemRoots   *bool
	tlsFingerprints  []string
	tlsCert          *string
	tlsKey           *string
	bind             *string
	versions         []string
	quicMaxStreams   *uint64
	websocketEnabled *bool
	websocketDelay   *time.Duration
	once             bool
	backoff          Backoff
	publish          *OriginProducer
	consume          *OriginProducer
}

func (c *clientConfig) ffi() (ffi.MoqClientConfig, error) {
	delay, err := optMicros("websocket delay", c.websocketDelay)
	if err != nil {
		return ffi.MoqClientConfig{}, err
	}
	backoff, err := c.backoff.ffi()
	if err != nil {
		return ffi.MoqClientConfig{}, err
	}
	cfg := ffi.MoqClientConfig{
		Bind:     c.bind,
		Versions: c.versions,
		Tls: ffi.MoqClientTls{
			Insecure:     c.tlsInsecure,
			Roots:        c.tlsRoots,
			SystemRoots:  c.tlsSystemRoots,
			Fingerprints: c.tlsFingerprints,
			Cert:         c.tlsCert,
			Key:          c.tlsKey,
		},
		Quic:      ffi.MoqQuicConfig{MaxStreams: c.quicMaxStreams},
		Websocket: ffi.MoqWebSocketConfig{Enabled: c.websocketEnabled, DelayUs: delay},
		Once:      c.once,
		Backoff:   backoff,
	}
	if c.publish != nil {
		cfg.Publish = &c.publish.inner
	}
	if c.consume != nil {
		cfg.Consume = &c.consume.inner
	}
	return cfg, nil
}

// Backoff is the retry pacing for automatic reconnects: the delay starts at
// Initial, multiplies by Multiplier after each failed attempt, and caps at Max.
// After Timeout of consecutive failures the connection gives up for good.
//
// Every field is optional: the zero value means the native default (1s, x2, 5s,
// and a 10s window), so a partial Backoff overrides only what it sets. Pass
// RetryForever as Timeout to keep retrying indefinitely.
type Backoff struct {
	Initial    time.Duration // delay before the first retry
	Multiplier uint32        // applied to the delay after each failure
	Max        time.Duration // ceiling on the delay
	Timeout    time.Duration // give up after this long
}

// RetryForever, passed as Backoff.Timeout, keeps a reconnecting session retrying
// indefinitely instead of giving up.
const RetryForever time.Duration = -1

// ffi leaves the zero fields unset, which is load-bearing rather than cosmetic:
// the native side reads a zero timeout as "retry forever" and a zero delay as no
// pacing at all, so passing Go's zero value straight through would turn
// Backoff{} into an unthrottled dial loop.
func (b Backoff) ffi() (ffi.MoqBackoff, error) {
	var out ffi.MoqBackoff
	var err error
	if out.InitialUs, err = backoffMicros("backoff initial", b.Initial); err != nil {
		return out, err
	}
	if out.MaxUs, err = backoffMicros("backoff max", b.Max); err != nil {
		return out, err
	}
	if b.Multiplier != 0 {
		out.Multiplier = &b.Multiplier
	}
	// Zero is the native encoding of "forever" and also Go's zero value, so the
	// two are spelled apart here: Backoff{} keeps the default and forever is
	// explicit at the call site.
	if b.Timeout == RetryForever {
		forever := uint64(0)
		out.TimeoutUs = &forever
	} else if out.TimeoutUs, err = backoffMicros("backoff timeout", b.Timeout); err != nil {
		return out, err
	}
	return out, nil
}

// backoffMicros leaves a zero duration unset and floors a positive one at 1us,
// so a sub-microsecond duration doesn't truncate to an unpaced zero.
func backoffMicros(name string, d time.Duration) (*uint64, error) {
	if d == 0 {
		return nil, nil
	}
	us, err := micros(name, d)
	if err != nil {
		return nil, err
	}
	us = max(us, 1)
	return &us, nil
}

// WithTLSVerify toggles TLS certificate verification. Verification is on by
// default; pass false only against a relay with a self-signed certificate
// during development.
func WithTLSVerify(verify bool) ClientOption {
	return func(c *clientConfig) { c.tlsInsecure = !verify }
}

// WithTLSRoots trusts PEM root certificate files instead of the system roots.
func WithTLSRoots(paths ...string) ClientOption {
	roots := append([]string(nil), paths...)
	return func(c *clientConfig) { c.tlsRoots = roots }
}

// WithTLSSystemRoots controls whether platform roots are trusted with custom roots.
func WithTLSSystemRoots(systemRoots bool) ClientOption {
	return func(c *clientConfig) { c.tlsSystemRoots = &systemRoots }
}

// WithTLSFingerprints pins the peer to one of these SHA-256 certificate fingerprints.
func WithTLSFingerprints(fingerprints ...string) ClientOption {
	pins := append([]string(nil), fingerprints...)
	return func(c *clientConfig) { c.tlsFingerprints = pins }
}

// WithClientTLSCert sets the path to a PEM certificate chain for mTLS.
func WithClientTLSCert(path string) ClientOption {
	return func(c *clientConfig) { c.tlsCert = &path }
}

// WithClientTLSKey sets the path to a PEM private key for mTLS.
func WithClientTLSKey(path string) ClientOption {
	return func(c *clientConfig) { c.tlsKey = &path }
}

// WithBind sets the local UDP socket bind address (default "[::]:0").
func WithBind(addr string) ClientOption {
	return func(c *clientConfig) { c.bind = &addr }
}

// WithVersions restricts the protocol versions offered, most preferred first,
// spelled like "moq-lite-03". By default every supported version is offered.
func WithVersions(versions ...string) ClientOption {
	offered := append([]string(nil), versions...)
	return func(c *clientConfig) { c.versions = offered }
}

// WithQUICMaxStreams caps the concurrent QUIC streams the peer may open toward
// this connection (default 1024). MoQ opens a stream per group, and for a
// subscriber those arrive from the relay, so a client subscribing to many tracks
// wants this raised. A publisher's own streams are bounded by the relay's
// advertised limit, not this one. Ignored by the WebSocket fallback.
func WithQUICMaxStreams(maxStreams uint64) ClientOption {
	return func(c *clientConfig) { c.quicMaxStreams = &maxStreams }
}

// WithWebSocketEnabled toggles the WebSocket fallback, which races QUIC for
// http(s) URLs. It is on by default; pass false against a relay that only
// serves QUIC.
func WithWebSocketEnabled(enabled bool) ClientOption {
	return func(c *clientConfig) { c.websocketEnabled = &enabled }
}

// WithWebSocketDelay sets the head start QUIC gets before the WebSocket
// fallback joins the race (default 200ms). Zero races both at once, and a
// negative delay fails Dial.
func WithWebSocketDelay(delay time.Duration) ClientOption {
	return func(c *clientConfig) { c.websocketDelay = &delay }
}

// WithReconnect toggles automatic reconnecting. It is on by default: the
// session redials with backoff whenever the transport drops, and broadcasts
// consumed through it ride out the gap. Pass false for a one-shot dial whose
// transport close ends the session.
func WithReconnect(enabled bool) ClientOption {
	return func(c *clientConfig) { c.once = !enabled }
}

// WithBackoff sets retry pacing for the automatic reconnect.
func WithBackoff(backoff Backoff) ClientOption {
	return func(c *clientConfig) { c.backoff = backoff }
}

// WithPublishOrigin sets the origin whose broadcasts are published to the
// remote. Pair with WithConsumeOrigin for full control; omit both to get a
// shared internal origin.
func WithPublishOrigin(o *OriginProducer) ClientOption {
	return func(c *clientConfig) { c.publish = o }
}

// WithConsumeOrigin sets the origin that receives broadcasts consumed from
// the remote.
func WithConsumeOrigin(o *OriginProducer) ClientOption {
	return func(c *clientConfig) { c.consume = o }
}

// Client is a connected MoQ client with automatic origin wiring. When no origin
// option is given, both sides share one origin, so a broadcast announced here is
// also discoverable here.
type Client struct {
	inner     *ffi.MoqClient
	publisher *OriginProducer
	consumer  *OriginConsumer
	session   *Session
	closeOnce sync.Once
	closeErr  error
}

// Dial connects to a MoQ server and returns the established client. Cancel ctx
// to abort an in-flight connect.
func Dial(ctx context.Context, url string, opts ...ClientOption) (*Client, error) {
	var cfg clientConfig
	for _, opt := range opts {
		opt(&cfg)
	}
	config, err := cfg.ffi()
	if err != nil {
		return nil, err
	}

	inner, err := ffi.NewMoqClient(config)
	if err != nil {
		return nil, err
	}
	c := &Client{inner: inner}

	session, err := bridge.CallHandle(ctx, inner.Cancel, func(ctx context.Context) (*ffi.MoqSession, error) {
		return inner.Connect(ctx, url)
	})
	if err != nil {
		inner.Cancel()
		return nil, err
	}
	c.session = &Session{inner: session}

	// The session always exposes both sides, wired from the options above or auto-created,
	// so publishing and discovery always have somewhere to go.
	c.publisher = c.session.Publish()
	c.consumer = c.session.Consume()

	return c, nil
}

// CreateBroadcast creates an unannounced broadcast at path, invisible to everyone until announced. Announce it after populating tracks.
//
// See [OriginProducer.CreateBroadcast].
func (c *Client) CreateBroadcast(path string) (*BroadcastProducer, error) {
	return c.publisher.CreateBroadcast(path)
}

// Announced streams routes announced by the remote under a pattern scope.
func (c *Client) Announced(options AnnounceOptions) (*AnnounceConsumer, error) {
	return c.consumer.Announced(options)
}

// AnnouncedBroadcast waits for a route covering path, then resolves the
// broadcast there.
func (c *Client) AnnouncedBroadcast(path string) (*AnnouncedBroadcast, error) {
	return c.consumer.AnnouncedBroadcast(path)
}

// RequestBroadcast resolves a broadcast at path as soon as it can be served: an
// existing exact-path broadcast, a covering announced route, or a dynamic
// fallback on the origin, or an error. Unlike AnnouncedBroadcast, it does not wait
// for a future announcement.
func (c *Client) RequestBroadcast(ctx context.Context, path string) (*BroadcastConsumer, error) {
	return c.consumer.RequestBroadcast(ctx, path)
}

// Session returns the underlying session. Hold the client (or session) to keep
// the connection alive.
func (c *Client) Session() *Session {
	return c.session
}

// Close gracefully shuts down the session and stops the client. Safe to call
// more than once.
func (c *Client) Close() error {
	c.closeOnce.Do(func() {
		if c.session != nil {
			c.closeErr = c.session.Shutdown(context.Background())
		}
		if c.inner != nil {
			c.inner.Cancel()
		}
	})
	return c.closeErr
}
