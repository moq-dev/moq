package moq

import (
	"context"
	"sync"
	"time"

	ffi "moq.dev/moq-ffi/moq"
)

// ClientOption configures a client created with Dial.
type ClientOption func(*clientConfig)

type clientConfig struct {
	tlsVerify          bool
	tlsRoots           []string
	tlsRootsSet        bool
	tlsSystemRoots     bool
	tlsSystemRootsSet  bool
	tlsFingerprints    []string
	tlsFingerprintsSet bool
	tlsCert            *string
	tlsKey             *string
	bind               *string
	reconnect          *bool
	backoff            *Backoff
	publish            *OriginProducer
	subscribe          *OriginProducer
}

// Backoff is the retry pacing for automatic reconnects: the delay starts at
// Initial, multiplies by Multiplier after each failed attempt, and caps at Max.
// After Timeout of consecutive failures the connection gives up for good.
//
// Every field is optional: the zero value means the default beside it, so a
// partial Backoff overrides only what it sets. Pass RetryForever as Timeout to
// keep retrying indefinitely.
type Backoff struct {
	Initial    time.Duration // delay before the first retry (default 1s)
	Multiplier uint32        // applied to the delay after each failure (default 2)
	Max        time.Duration // ceiling on the delay (default 30s)
	Timeout    time.Duration // give up after this long (default 5m)
}

// RetryForever, passed as Backoff.Timeout, keeps a reconnecting session retrying
// indefinitely instead of giving up.
const RetryForever time.Duration = -1

const (
	defaultBackoffInitial    = time.Second
	defaultBackoffMultiplier = 2
	defaultBackoffMax        = 30 * time.Second
	defaultBackoffTimeout    = 5 * time.Minute
)

// ffi resolves the unset fields, which is load-bearing rather than cosmetic:
// the native side reads a zero timeout as "retry forever" and a zero delay as
// no pacing at all, so passing Go's zero value straight through would turn
// Backoff{} into an unthrottled dial loop.
func (b Backoff) ffi() ffi.MoqBackoff {
	multiplier := b.Multiplier
	if multiplier == 0 {
		multiplier = defaultBackoffMultiplier
	}

	// Zero is the native encoding of "forever" and also Go's zero value, so the
	// two are spelled apart here: Backoff{} keeps the documented default and
	// forever is explicit at the call site.
	timeoutMs := backoffMs(b.Timeout, defaultBackoffTimeout)
	if b.Timeout == RetryForever {
		timeoutMs = 0
	}

	return ffi.MoqBackoff{
		InitialMs:  backoffMs(b.Initial, defaultBackoffInitial),
		Multiplier: multiplier,
		MaxMs:      backoffMs(b.Max, defaultBackoffMax),
		TimeoutMs:  timeoutMs,
	}
}

// backoffMs converts d to milliseconds, substituting def when it is unset or
// negative (a negative would wrap when cast to uint64) and flooring at 1ms so a
// sub-millisecond duration doesn't truncate to an unpaced zero.
func backoffMs(d, def time.Duration) uint64 {
	if d <= 0 {
		d = def
	}
	ms := d.Milliseconds()
	if ms < 1 {
		ms = 1
	}
	return uint64(ms)
}

// WithTLSVerify toggles TLS certificate verification. Verification is on by
// default; pass false only against a relay with a self-signed certificate
// during development.
func WithTLSVerify(verify bool) ClientOption {
	return func(c *clientConfig) { c.tlsVerify = verify }
}

// WithTLSRoots trusts PEM root certificate files instead of the system roots.
func WithTLSRoots(paths ...string) ClientOption {
	roots := append([]string(nil), paths...)
	return func(c *clientConfig) {
		c.tlsRoots = roots
		c.tlsRootsSet = true
	}
}

// WithTLSSystemRoots controls whether platform roots are trusted with custom roots.
func WithTLSSystemRoots(systemRoots bool) ClientOption {
	return func(c *clientConfig) {
		c.tlsSystemRoots = systemRoots
		c.tlsSystemRootsSet = true
	}
}

// WithTLSFingerprints pins the peer to one of these SHA-256 certificate fingerprints.
func WithTLSFingerprints(fingerprints ...string) ClientOption {
	pins := append([]string(nil), fingerprints...)
	return func(c *clientConfig) {
		c.tlsFingerprints = pins
		c.tlsFingerprintsSet = true
	}
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

// WithReconnect toggles automatic reconnecting. It is on by default: the
// session redials with backoff whenever the transport drops, and broadcasts
// consumed through it ride out the gap. Pass false for a one-shot dial whose
// transport close ends the session.
func WithReconnect(enabled bool) ClientOption {
	return func(c *clientConfig) { c.reconnect = &enabled }
}

// WithBackoff sets retry pacing for the automatic reconnect.
func WithBackoff(backoff Backoff) ClientOption {
	return func(c *clientConfig) { c.backoff = &backoff }
}

// WithPublishOrigin sets the origin whose broadcasts are published to the
// remote. Pair with WithSubscribeOrigin for full control; omit both to get a
// shared internal origin.
func WithPublishOrigin(o *OriginProducer) ClientOption {
	return func(c *clientConfig) { c.publish = o }
}

// WithSubscribeOrigin sets the origin that receives broadcasts consumed from
// the remote.
func WithSubscribeOrigin(o *OriginProducer) ClientOption {
	return func(c *clientConfig) { c.subscribe = o }
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
}

// Dial connects to a MoQ server and returns the established client. Cancel ctx
// to abort an in-flight connect.
func Dial(ctx context.Context, url string, opts ...ClientOption) (*Client, error) {
	// Verification is on unless WithTLSVerify(false) says otherwise; the zero
	// value would mean the opposite.
	cfg := clientConfig{tlsVerify: true}
	for _, opt := range opts {
		opt(&cfg)
	}

	c := &Client{}
	inner := ffi.NewMoqClient()
	if !cfg.tlsVerify {
		inner.SetTlsDisableVerify(true)
	}
	if cfg.tlsRootsSet {
		inner.SetTlsRoots(cfg.tlsRoots)
	}
	if cfg.tlsSystemRootsSet {
		inner.SetTlsSystemRoots(cfg.tlsSystemRoots)
	}
	if cfg.tlsFingerprintsSet {
		inner.SetTlsFingerprints(cfg.tlsFingerprints)
	}
	if cfg.tlsCert != nil {
		inner.SetTlsCert(cfg.tlsCert)
	}
	if cfg.tlsKey != nil {
		inner.SetTlsKey(cfg.tlsKey)
	}
	if cfg.bind != nil {
		if err := inner.SetBind(*cfg.bind); err != nil {
			inner.Cancel()
			return nil, err
		}
	}
	if cfg.reconnect != nil {
		inner.SetReconnect(*cfg.reconnect)
	}
	if cfg.backoff != nil {
		inner.SetBackoff(cfg.backoff.ffi())
	}
	if cfg.publish != nil {
		inner.SetPublish(&cfg.publish.inner)
	}
	if cfg.subscribe != nil {
		inner.SetConsume(&cfg.subscribe.inner)
	}
	c.inner = inner

	session, err := runCancellable(ctx, inner.Cancel, func() (*ffi.MoqSession, error) {
		return inner.Connect(url)
	})
	if err != nil {
		inner.Cancel()
		return nil, err
	}
	c.session = &Session{inner: session}

	// The session always exposes both sides, wired from the options above or auto-created,
	// so publishing and discovery always have somewhere to go.
	c.publisher = c.session.Publisher()
	c.consumer = c.session.Consumer()

	return c, nil
}

// CreateBroadcast creates a live broadcast at path so the remote can discover it.
//
// See [OriginProducer.CreateBroadcast].
func (c *Client) CreateBroadcast(path string) (*BroadcastProducer, error) {
	return c.publisher.CreateBroadcast(path)
}

// Announced streams broadcasts announced by the remote under prefix.
func (c *Client) Announced(prefix string) (*Announced, error) {
	return c.consumer.Announced(prefix)
}

// AnnouncedBroadcast resolves a single announced broadcast at path.
func (c *Client) AnnouncedBroadcast(path string) (*AnnouncedBroadcast, error) {
	return c.consumer.AnnouncedBroadcast(path)
}

// RequestBroadcast resolves a broadcast at path as soon as it can be served: the
// announced broadcast if present, otherwise a dynamic fallback on the origin, or an
// error. Unlike AnnouncedBroadcast, it does not wait for a future announcement.
func (c *Client) RequestBroadcast(path string) (*BroadcastConsumer, error) {
	return c.consumer.RequestBroadcast(path)
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
			c.session.Shutdown()
		}
		if c.inner != nil {
			c.inner.Cancel()
		}
	})
	return nil
}
