package moq

import (
	"context"
	"sync"

	ffi "github.com/moq-dev/moq-go-ffi/moq"
)

// ClientOption configures a client created with Dial.
type ClientOption func(*clientConfig)

type clientConfig struct {
	tlsDisableVerify bool
	bind             *string
	publish          *OriginProducer
	subscribe        *OriginProducer
}

// WithTLSDisableVerify disables TLS certificate verification (development only).
func WithTLSDisableVerify() ClientOption {
	return func(c *clientConfig) { c.tlsDisableVerify = true }
}

// WithBind sets the local UDP socket bind address (default "[::]:0").
func WithBind(addr string) ClientOption {
	return func(c *clientConfig) { c.bind = &addr }
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
// option is given, Dial creates a shared internal origin used for both
// publishing and consuming.
type Client struct {
	inner         *ffi.MoqClient
	origin        *OriginProducer
	publishOrigin *OriginProducer
	consumeOrigin *OriginProducer
	consumer      *OriginConsumer
	session       *Session
	closeOnce     sync.Once
}

// Dial connects to a MoQ server and returns the established client. Cancel ctx
// to abort an in-flight connect.
func Dial(ctx context.Context, url string, opts ...ClientOption) (*Client, error) {
	var cfg clientConfig
	for _, opt := range opts {
		opt(&cfg)
	}

	c := &Client{}
	if cfg.publish == nil && cfg.subscribe == nil {
		c.origin = NewOriginProducer()
		c.publishOrigin = c.origin
		c.consumeOrigin = c.origin
	} else {
		c.publishOrigin = cfg.publish
		c.consumeOrigin = cfg.subscribe
	}

	inner := ffi.NewMoqClient()
	if cfg.tlsDisableVerify {
		inner.SetTlsDisableVerify(true)
	}
	if cfg.bind != nil {
		if err := inner.SetBind(*cfg.bind); err != nil {
			inner.Cancel()
			return nil, err
		}
	}
	if c.publishOrigin != nil {
		inner.SetPublish(&c.publishOrigin.inner)
	}
	if c.consumeOrigin != nil {
		inner.SetConsume(&c.consumeOrigin.inner)
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

	// Only a configured consume origin yields a consumer. A publish-only client
	// has none, so Announced/AnnouncedBroadcast surface ErrNoConsumeOrigin
	// rather than silently reading from the local publish origin.
	if c.consumeOrigin != nil {
		c.consumer = c.consumeOrigin.Consume()
	}

	return c, nil
}

// Publish publishes a broadcast under path, served to the remote.
func (c *Client) Publish(path string, broadcast *BroadcastProducer) error {
	if c.publishOrigin == nil {
		return ErrNoPublishOrigin
	}
	return c.publishOrigin.Publish(path, broadcast)
}

// Announced streams broadcasts announced by the remote under prefix.
func (c *Client) Announced(prefix string) (*Announced, error) {
	if c.consumer == nil {
		return nil, ErrNoConsumeOrigin
	}
	return c.consumer.Announced(prefix)
}

// AnnouncedBroadcast resolves a single announced broadcast at path.
func (c *Client) AnnouncedBroadcast(path string) (*AnnouncedBroadcast, error) {
	if c.consumer == nil {
		return nil, ErrNoConsumeOrigin
	}
	return c.consumer.AnnouncedBroadcast(path)
}

// RequestBroadcast resolves a broadcast at path as soon as it can be served: the
// announced broadcast if present, otherwise a dynamic fallback on the origin, or an
// error. Unlike AnnouncedBroadcast, it does not wait for a future announcement.
func (c *Client) RequestBroadcast(path string) (*BroadcastConsumer, error) {
	if c.consumer == nil {
		return nil, ErrNoConsumeOrigin
	}
	return c.consumer.RequestBroadcast(path)
}

// Session returns the underlying session. Hold the client (or session) to keep
// the connection alive.
func (c *Client) Session() *Session {
	return c.session
}

// Close stops the client. In-flight sessions stay alive until their handles are
// dropped or cancelled. Safe to call more than once.
func (c *Client) Close() error {
	c.closeOnce.Do(func() {
		if c.inner != nil {
			c.inner.Cancel()
		}
	})
	return nil
}
