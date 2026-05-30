package moq

import (
	"context"

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

	session, err := runCancellable(ctx, inner.Cancel, func() (*Session, error) {
		return inner.Connect(url)
	})
	if err != nil {
		inner.Cancel()
		return nil, err
	}
	c.session = session

	// Build a consumer from whichever origin handles consuming.
	if origin := c.consumeOrigin; origin != nil {
		c.consumer = origin.Consume()
	} else if origin := c.publishOrigin; origin != nil {
		c.consumer = origin.Consume()
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

// Session returns the underlying session. Hold the client (or session) to keep
// the connection alive.
func (c *Client) Session() *Session {
	return c.session
}

// Close stops the client. In-flight sessions stay alive until their handles are
// dropped or cancelled.
func (c *Client) Close() error {
	if c.inner != nil {
		c.inner.Cancel()
		c.inner = nil
	}
	c.consumer = nil
	c.session = nil
	return nil
}
