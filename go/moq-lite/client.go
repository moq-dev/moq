package moq

import (
	"context"
	"fmt"

	moqffi "github.com/moq-dev/moq/go/moq-ffi/moq"
)

// ClientOptions configures a Client.
type ClientOptions struct {
	// TLSVerify enables certificate verification. Defaults to true.
	// Set to false for local development with self-signed certs.
	TLSVerify *bool

	// Publish is the origin to publish local broadcasts to the remote.
	// If nil and Subscribe is nil, an internal origin is created and used
	// for both.
	Publish *OriginProducer

	// Subscribe is the origin to consume remote broadcasts into. If nil
	// and Publish is nil, an internal origin is created and used for both.
	Subscribe *OriginProducer
}

// Client is a high-level MoQ client with automatic origin wiring.
//
// Simple usage (auto-managed origin):
//
//	client, err := moq.Connect(ctx, "https://relay.example.com", nil)
//	defer client.Close()
//	for ann, err := range client.Announced(ctx, "").Announcements(ctx) { ... }
type Client struct {
	inner             *moqffi.MoqClient
	session           *moqffi.MoqSession
	publishOrigin     *OriginProducer
	consumeOrigin     *OriginProducer
	consumer          *OriginConsumer
	ownsPublishOrigin bool
}

// Connect dials a MoQ relay and returns a connected Client.
//
// `opts` may be nil for default settings.
func Connect(ctx context.Context, url string, opts *ClientOptions) (*Client, error) {
	if opts == nil {
		opts = &ClientOptions{}
	}

	c := &Client{
		inner:         moqffi.NewMoqClient(),
		publishOrigin: opts.Publish,
		consumeOrigin: opts.Subscribe,
	}

	if opts.Publish == nil && opts.Subscribe == nil {
		shared := NewOriginProducer()
		c.publishOrigin = shared
		c.consumeOrigin = shared
		c.ownsPublishOrigin = true
	}

	if opts.TLSVerify != nil && !*opts.TLSVerify {
		c.inner.SetTlsDisableVerify(true)
	}

	if c.publishOrigin != nil {
		inner := c.publishOrigin.inner
		c.inner.SetPublish(&inner)
	}
	if c.consumeOrigin != nil {
		inner := c.consumeOrigin.inner
		c.inner.SetConsume(&inner)
	}

	session, err := callCtx(ctx, c.inner.Cancel, func() (*moqffi.MoqSession, error) {
		return c.inner.Connect(url)
	})
	if err != nil {
		c.cleanup()
		return nil, fmt.Errorf("connect %s: %w", url, err)
	}
	c.session = session

	origin := c.consumeOrigin
	if origin == nil {
		origin = c.publishOrigin
	}
	if origin != nil {
		c.consumer = origin.Consume()
	}

	return c, nil
}

// Publish announces a local broadcast at the given path.
func (c *Client) Publish(path string, broadcast *BroadcastProducer) error {
	if c.publishOrigin == nil {
		return fmt.Errorf("no publish origin configured")
	}
	return c.publishOrigin.Publish(path, broadcast)
}

// Announced returns an iterator over all broadcasts under `prefix`.
func (c *Client) Announced(prefix string) (*Announced, error) {
	if c.consumer == nil {
		return nil, fmt.Errorf("no consume origin configured")
	}
	return c.consumer.Announced(prefix)
}

// AnnouncedBroadcast waits for a specific broadcast by path.
func (c *Client) AnnouncedBroadcast(path string) (*AnnouncedBroadcast, error) {
	if c.consumer == nil {
		return nil, fmt.Errorf("no consume origin configured")
	}
	return c.consumer.AnnouncedBroadcast(path)
}

// Session returns the underlying MoqSession. Closed() blocks until the
// session ends.
func (c *Client) Session() *moqffi.MoqSession { return c.session }

// Closed blocks until the session is closed.
func (c *Client) Closed(ctx context.Context) error {
	if c.session == nil {
		return nil
	}
	_, err := callCtx(ctx, func() { c.session.Cancel(0) }, func() (struct{}, error) {
		return struct{}{}, c.session.Closed()
	})
	return err
}

func (c *Client) cleanup() {
	if c.consumer != nil {
		c.consumer.Close()
		c.consumer = nil
	}
	if c.session != nil {
		c.session.Destroy()
		c.session = nil
	}
	if c.inner != nil {
		c.inner.Cancel()
		c.inner.Destroy()
		c.inner = nil
	}
	if c.ownsPublishOrigin && c.publishOrigin != nil {
		c.publishOrigin.Close()
		c.publishOrigin = nil
	}
}

// Close releases resources held by the client. After Close, the client is
// unusable.
func (c *Client) Close() { c.cleanup() }
