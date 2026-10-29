package moq

import (
	"context"
	"runtime/debug"
	"strings"
	"sync"
	"time"

	ffi "moq.dev/moq-ffi/moq"
	"moq.dev/moq/internal/bridge"
)

// implementation is this module's product token, which its sessions advertise
// ahead of moq-ffi's. The module version comes from the build info Go embeds in
// every binary; a source-tree build, which has none, reports dev.
var implementation = func() string {
	version := "dev"
	if info, ok := debug.ReadBuildInfo(); ok {
		for _, module := range append([]*debug.Module{&info.Main}, info.Deps...) {
			if module.Path == "moq.dev/moq" && module.Replace == nil && strings.HasPrefix(module.Version, "v") {
				version = strings.TrimPrefix(module.Version, "v")
			}
		}
	}
	return "moq-go/" + version
}()

// ClientConfig configures a client created with Dial, mirroring moq-ffi's
// MoqClientConfig. The zero value dials with the native defaults: an empty
// string, a zero MaxStreams, or a nil pointer keeps each one.
type ClientConfig struct {
	// Bind is the local UDP address to bind, e.g. "0.0.0.0:0". Empty binds an
	// ephemeral dual-stack port.
	Bind string
	// Versions restricts the protocol versions offered, most preferred first,
	// spelled like "moq-lite-03". Empty offers every supported version, which
	// the Versions function lists.
	Versions []string
	// Tokens are the credentials every dial presents. The first is the
	// connection credential, unless the URL carries its own ?jwt=; each other
	// one widens the session's grant. Replace them with Session.Tokens.
	Tokens []string
	// Timeout bounds one connect, dial and handshake together. Zero waits forever.
	Timeout *time.Duration
	// Race is the Happy Eyeballs stagger: how long before the next resolved
	// address is also dialed. Zero dials every address at once.
	Race *time.Duration
	// ResolutionDelay is how long IPv4 waits for the full DNS answer before
	// dialing alone (Happy Eyeballs).
	ResolutionDelay *time.Duration
	// TLS sets certificate trust and the mTLS identity.
	TLS ClientTLS
	// QUIC tunes the QUIC transport; the WebSocket fallback ignores it.
	QUIC QUICConfig
	// WebSocket configures the fallback raced against QUIC for http(s) URLs.
	WebSocket WebSocketConfig
	// Once dials a single time instead of redialing with Backoff whenever the
	// transport drops, so the transport's close ends the session.
	Once bool
	// Backoff paces the automatic reconnect.
	Backoff Backoff
	// Publish is the origin whose broadcasts are published to the remote.
	// With neither Publish nor Consume set, both sides share one fresh origin,
	// so a broadcast announced here is also discoverable here.
	Publish *OriginProducer
	// Consume is the origin that receives broadcasts consumed from the remote.
	Consume *OriginProducer
}

// ClientTLS is the certificate trust and mTLS identity for a ClientConfig.
type ClientTLS struct {
	// Insecure skips certificate verification. Local development only.
	Insecure bool
	// Roots are PEM root certificate files to trust instead of the platform roots.
	Roots []string
	// SystemRoots is whether to also trust the platform roots. Nil trusts them
	// only when Roots is empty.
	SystemRoots *bool
	// Fingerprints pins the peer to one of these hex SHA-256 certificate
	// fingerprints, such as Server.CertFingerprints reports.
	Fingerprints []string
	// Cert is the path to a PEM certificate chain to present for mTLS. Pair with Key.
	Cert string
	// Key is the path to a PEM private key to present for mTLS. Pair with Cert.
	Key string
	// HostName is the server name to verify the certificate against (SNI)
	// instead of the URL's host, so a relay can be dialed by IP.
	HostName string
}

// QUICConfig tunes the QUIC transport. Nil fields leave the choice to the
// library or backend.
type QUICConfig struct {
	// MaxStreams caps the concurrent QUIC streams the peer may open toward this
	// connection; zero keeps the default. MoQ opens a stream per group, and for
	// a subscriber those arrive from the relay, so a client subscribing to many
	// tracks wants this raised.
	MaxStreams uint64
	// IdleTimeout is how long an idle connection survives before it is dropped.
	IdleTimeout *time.Duration
	// KeepAlive is the keep-alive ping interval. Zero disables it.
	KeepAlive *time.Duration
	// GSO toggles UDP generic segmentation offload, which batches sends into one syscall.
	GSO *bool
	// MTUDiscovery toggles path MTU discovery.
	MTUDiscovery *bool
	// CongestionControl picks the congestion control family.
	CongestionControl *CongestionControl
	// Qlog is a directory, which must exist, to write qlog traces into. Dial
	// fails unless QlogSupported.
	Qlog string
	// Keylog writes TLS session secrets to the file SSLKEYLOGFILE names, for
	// decrypting a capture. Without SSLKEYLOGFILE the dial fails.
	Keylog bool
}

// WebSocketConfig is the WebSocket fallback, raced against QUIC for http(s) URLs.
type WebSocketConfig struct {
	// Enabled toggles the fallback; nil enables it. Disable it against a relay
	// that only serves QUIC, so a failed QUIC dial reports its own error.
	Enabled *bool
	// Delay is the head start QUIC gets before the fallback joins the race.
	// Nil uses 200ms, and zero races both at once.
	Delay *time.Duration
}

func (c ClientConfig) ffi() (ffi.MoqClientConfig, error) {
	backoff, err := c.Backoff.ffi()
	if err != nil {
		return ffi.MoqClientConfig{}, err
	}
	// The generated converter panics on a negative duration, so the first one fails the whole config.
	for _, d := range []struct {
		name  string
		value *time.Duration
	}{
		{"connect timeout", c.Timeout},
		{"race", c.Race},
		{"resolution delay", c.ResolutionDelay},
		{"QUIC idle timeout", c.QUIC.IdleTimeout},
		{"QUIC keep-alive", c.QUIC.KeepAlive},
		{"websocket delay", c.WebSocket.Delay},
	} {
		if err := bridge.NonNegative(d.value, d.name); err != nil {
			return ffi.MoqClientConfig{}, err
		}
	}
	// Go's empty string is the native null: none of these fields accepts "".
	str := func(s string) *string {
		if s == "" {
			return nil
		}
		return &s
	}
	// The native side refuses zero streams and reads a null keylog as off, so
	// Go's zero values map to null like the strings above.
	var maxStreams *uint64
	if c.QUIC.MaxStreams != 0 {
		maxStreams = &c.QUIC.MaxStreams
	}
	var keylog *bool
	if c.QUIC.Keylog {
		keylog = &c.QUIC.Keylog
	}
	cfg := ffi.MoqClientConfig{
		Bind:              str(c.Bind),
		Versions:          c.Versions,
		Tokens:          c.Tokens,
		Timeout:         c.Timeout,
		Race:            c.Race,
		ResolutionDelay: c.ResolutionDelay,
		Tls: ffi.MoqClientTls{
			Insecure:     c.TLS.Insecure,
			Roots:        c.TLS.Roots,
			SystemRoots:  c.TLS.SystemRoots,
			Fingerprints: c.TLS.Fingerprints,
			Cert:         str(c.TLS.Cert),
			Key:          str(c.TLS.Key),
			HostName:     str(c.TLS.HostName),
		},
		Quic: ffi.MoqQuicConfig{
			MaxStreams:        maxStreams,
			IdleTimeout:       c.QUIC.IdleTimeout,
			KeepAlive:         c.QUIC.KeepAlive,
			Gso:               c.QUIC.GSO,
			MtuDiscovery:      c.QUIC.MTUDiscovery,
			CongestionControl: c.QUIC.CongestionControl,
			Qlog:              str(c.QUIC.Qlog),
			Keylog:            keylog,
		},
		Websocket:      ffi.MoqWebSocketConfig{Enabled: c.WebSocket.Enabled, Delay: c.WebSocket.Delay},
		Once:           c.Once,
		Backoff:        backoff,
		Implementation: &implementation,
	}
	if c.Publish != nil {
		cfg.Publish = &c.Publish.inner
	}
	if c.Consume != nil {
		cfg.Consume = &c.Consume.inner
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
	if out.Initial, err = backoffDuration("backoff initial", b.Initial); err != nil {
		return out, err
	}
	if out.Max, err = backoffDuration("backoff max", b.Max); err != nil {
		return out, err
	}
	if b.Multiplier != 0 {
		out.Multiplier = &b.Multiplier
	}
	// Zero is the native encoding of "forever" and also Go's zero value, so the
	// two are spelled apart here: Backoff{} keeps the default and forever is
	// explicit at the call site.
	if b.Timeout == RetryForever {
		forever := time.Duration(0)
		out.Timeout = &forever
	} else if out.Timeout, err = backoffDuration("backoff timeout", b.Timeout); err != nil {
		return out, err
	}
	return out, nil
}

// backoffDuration leaves a zero duration unset and refuses a negative one.
func backoffDuration(name string, d time.Duration) (*time.Duration, error) {
	if d == 0 {
		return nil, nil
	}
	return &d, bridge.NonNegative(&d, name)
}

// Client is a connected MoQ client with automatic origin wiring. When no origin
// is configured, both sides share one origin, so a broadcast announced here is
// also discoverable here.
type Client struct {
	inner     *ffi.MoqClient
	publisher *OriginProducer
	consumer  *OriginConsumer
	session   *Session
	closeOnce sync.Once
	closeErr  error
}

// Dial starts a MoQ connection and returns without waiting for its first native dial to succeed. Cancel ctx
// to abort an in-flight connect.
func Dial(ctx context.Context, url string, config ClientConfig) (*Client, error) {
	native, err := config.ffi()
	if err != nil {
		return nil, err
	}

	inner, err := ffi.NewMoqClient(native)
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

	// The session always exposes both sides, wired from the config above or auto-created,
	// so publishing and discovery always have somewhere to go.
	c.publisher = c.session.Publish()
	c.consumer = c.session.Consume()

	return c, nil
}

// CreateBroadcast creates an unannounced broadcast at path, invisible to everyone until announced. Announce it after populating tracks.
//
// See [OriginProducer.CreateBroadcast].
func (c *Client) CreateBroadcast(path string) (*Publication, error) {
	return c.publisher.CreateBroadcast(path)
}

// Announced streams routes announced by the remote under a pattern scope.
func (c *Client) Announced(options AnnounceOptions) (*AnnounceConsumer, error) {
	return c.consumer.Announced(options)
}

// AnnouncedBroadcast follows path, resolving each publisher instance that serves
// it.
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
			c.session.Shutdown()
			c.closeErr = c.session.Closed(context.Background())
		}
		if c.inner != nil {
			c.inner.Cancel()
		}
	})
	return c.closeErr
}
