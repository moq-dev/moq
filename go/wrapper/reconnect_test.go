package moq_test

import (
	"context"
	"errors"
	"fmt"
	"net"
	"sync"
	"testing"
	"time"

	"moq.dev/moq"
)

// reconnectTimeout bounds the relay-restart test; its backoff floor is 50ms, so
// even a slow CI machine reconnects well inside this.
const reconnectTimeout = 30 * time.Second

func ptr[T any](v T) *T { return &v }

// relay is a local MoQ server plus the sessions it accepted, so a test can drop
// both the way a fleet deploy does and bring a replacement up on the same port.
type relay struct {
	server *moq.Server
	addr   string

	mu       sync.Mutex
	sessions []*moq.Session
}

// startRelay binds a relay at addr (an ephemeral loopback port when addr is
// host:0) and accepts sessions until ctx ends. Close releases the socket before
// it returns, so a restart reuses the port with no retry.
func startRelay(t *testing.T, ctx context.Context, addr string) *relay {
	t.Helper()

	server, err := moq.Listen(ctx, addr, moq.WithTLSGenerate("localhost"))
	if err != nil {
		t.Fatalf("relay did not bind %s: %v", addr, err)
	}

	r := &relay{server: server, addr: server.LocalAddr()}
	go func() {
		for req, err := range server.All(ctx) {
			if err != nil {
				return
			}
			session, err := req.Accept(ctx, nil, nil)
			if err != nil {
				continue
			}
			r.mu.Lock()
			r.sessions = append(r.sessions, session)
			r.mu.Unlock()
			// Hold the session until it closes, like Server.Serve does.
			go func() { _ = session.Closed(ctx) }()
		}
	}()
	return r
}

// restart drops every session and the listener, then binds a fresh relay on the
// same port: the client-visible shape of a fleet deploy.
func (r *relay) restart(t *testing.T, ctx context.Context) *relay {
	t.Helper()

	r.mu.Lock()
	for _, session := range r.sessions {
		session.Cancel(0)
	}
	r.sessions = nil
	r.mu.Unlock()

	_ = r.server.Close()
	return startRelay(t, ctx, r.addr)
}

// dialWorker connects as a Worker would: automatic reconnects with fast retries
// that never give up, against a relay with a self-signed certificate.
func dialWorker(t *testing.T, ctx context.Context, url string) *moq.Client {
	t.Helper()

	client, err := moq.Dial(ctx, url, moq.ClientConfig{
		TLS: moq.ClientTLS{Insecure: true},
		Backoff: moq.Backoff{
			Initial: 50 * time.Millisecond,
			Max:     time.Second,
			Timeout: moq.RetryForever,
		},
	})
	if err != nil {
		t.Fatal(err)
	}
	return client
}

// waitConnects blocks until the session reports at least want connections, proving
// the worker redialed rather than just reusing the transport.
func waitConnects(t *testing.T, ctx context.Context, session *moq.Session, want uint64) {
	t.Helper()

	state := session.State()
	for state.Connects < want {
		var err error
		state, err = session.StateChanged(ctx, state.Revision)
		if err != nil {
			t.Fatalf("wait connects: %v", err)
		}
	}
}

// awaitAnnouncement drains announcements until an active one names path.
func awaitAnnouncement(t *testing.T, ctx context.Context, announced *moq.AnnounceConsumer, path string) {
	t.Helper()

	for {
		event, err := announced.Next(ctx)
		if err != nil {
			t.Fatalf("waiting for %q: %v", path, err)
		}
		if event == nil {
			t.Fatalf("announcement stream ended before %q", path)
		}
		if ann, ok := event.(moq.AnnounceEventStart); ok && ann.Announce.Prefix == path {
			return
		}
	}
}

// Close releases the listening socket before it returns, so the same address
// binds again on the first try. A retry here would hide the async teardown the
// restart above relies on.
func TestServerCloseReleasesPort(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), reconnectTimeout)
	defer cancel()

	first, err := moq.Listen(ctx, "127.0.0.1:0", moq.WithTLSGenerate("localhost"))
	if err != nil {
		t.Fatal(err)
	}
	addr := first.LocalAddr()
	if err := first.Close(); err != nil {
		t.Fatal(err)
	}

	// No retry: the port is already free.
	second, err := moq.Listen(ctx, addr, moq.WithTLSGenerate("localhost"))
	if err != nil {
		t.Fatalf("rebind %s: %v", addr, err)
	}
	defer func() { _ = second.Close() }()
}

// The keylog opt-in reaches the native server, which refuses it without SSLKEYLOGFILE.
func TestListenKeylogRequiresSSLKEYLOGFILE(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), reconnectTimeout)
	defer cancel()
	t.Setenv("SSLKEYLOGFILE", "")

	server, err := moq.Listen(ctx, "127.0.0.1:0", moq.WithTLSGenerate("localhost"), moq.WithServerQUICKeylog(false))
	if err != nil {
		t.Fatal(err)
	}
	_ = server.Close()

	if _, err := moq.Listen(ctx, "127.0.0.1:0", moq.WithTLSGenerate("localhost"), moq.WithServerQUICKeylog(true)); !errors.Is(err, moq.ErrConfig) {
		t.Fatalf("keylog without SSLKEYLOGFILE: got %v, want ErrConfig", err)
	}
}

// A Go worker survives a relay restart the way a moq-c worker does: the session
// redials with backoff, the publisher re-announces its broadcast to the fresh
// relay, and the subscriber's subscription receives frames again. The connect count
// pairs with Status so a worker can log each reconnect.
func TestReconnectAcrossRelayRestart(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), reconnectTimeout)
	defer cancel()

	current := startRelay(t, ctx, "127.0.0.1:0")
	url := "https://" + current.addr

	// Publisher: a continuous track keeps a frame available on the far side of
	// the restart, since an unwatched live track drops what it writes.
	publisher := dialWorker(t, ctx, url)
	defer publisher.Close()

	publication, err := publisher.CreateBroadcast("live")
	if err != nil {
		t.Fatal(err)
	}
	broadcast := publication.Broadcast()
	defer broadcast.Close()

	track, err := broadcast.PublishTrack("data", timedInfo())
	if err != nil {
		t.Fatal(err)
	}
	defer track.Finish()
	if err := publication.Announce(moq.AnnounceRoute{}); err != nil {
		t.Fatal(err)
	}

	stop := make(chan struct{})
	defer close(stop)
	go func() {
		for i := 0; ; i++ {
			select {
			case <-stop:
				return
			default:
			}
			_ = track.WriteFrame(moq.Frame{Payload: fmt.Appendf(nil, "frame-%d", i), Timestamp: ts(0)})
			time.Sleep(10 * time.Millisecond)
		}
	}()

	// Subscriber: finds the broadcast and reads a frame before the restart, so
	// the post-restart read proves delivery resumed rather than never started.
	subscriber := dialWorker(t, ctx, url)
	defer subscriber.Close()

	announced, err := subscriber.Announced(moq.AnnounceOptions{})
	if err != nil {
		t.Fatal(err)
	}
	defer announced.Cancel()

	awaitAnnouncement(t, ctx, announced, "live")

	before, err := subscriber.RequestBroadcast(ctx, "live")
	if err != nil {
		t.Fatal(err)
	}
	trackConsumer, err := before.SubscribeTrack(ctx, "data", nil)
	if err != nil {
		t.Fatal(err)
	}
	defer trackConsumer.Cancel()

	if frame, err := trackConsumer.ReadFrame(ctx); err != nil || frame == nil {
		t.Fatalf("frame before restart: frame=%+v err=%v", frame, err)
	}
	if err := subscriber.Session().State().Failure; err != nil {
		t.Fatalf("failure before any drop: %v", err)
	}
	if version := subscriber.Session().Stats().Version; version == nil || *version == "" {
		t.Fatal("a live session must report its negotiated version")
	}

	// Drop every session and the listener, then bind a replacement on the same
	// port: a deploy, with an origin the replacement has never seen.
	current = current.restart(t, ctx)
	defer func() { _ = current.server.Close() }()

	// Both workers observe the reconnect and advance their connect count past the first.
	waitConnects(t, ctx, publisher.Session(), 2)
	waitConnects(t, ctx, subscriber.Session(), 2)
	// The connect that advanced the count also cleared the drop's reason.
	if err := subscriber.Session().State().Failure; err != nil {
		t.Fatalf("failure after reconnect: %v", err)
	}

	// The publisher re-announces the broadcast the fresh relay never saw.
	awaitAnnouncement(t, ctx, announced, "live")

	// The subscriber resolves the re-announced broadcast again and receives
	// frames, which is how a worker follows a relay restart: the old handle's
	// broadcast closed with its source, and the announcement is what re-anchors.
	after, err := subscriber.RequestBroadcast(ctx, "live")
	if err != nil {
		t.Fatal(err)
	}
	resumed, err := after.SubscribeTrack(ctx, "data", nil)
	if err != nil {
		t.Fatal(err)
	}
	defer resumed.Cancel()

	frame, err := resumed.ReadFrame(ctx)
	if err != nil {
		t.Fatalf("frame after restart: %v", err)
	}
	if frame == nil {
		t.Fatal("frame after restart: nil")
	}
}

// The WebSocket fallback knobs reach the native client: a QUIC-only dial with
// no head start still connects, and a negative delay fails Dial instead of
// wrapping into an enormous one.
func TestDialWebSocketOptions(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), reconnectTimeout)
	defer cancel()

	r := startRelay(t, ctx, "127.0.0.1:0")
	defer r.server.Close()

	url := "https://" + r.addr
	client, err := moq.Dial(ctx, url, moq.ClientConfig{
		TLS:       moq.ClientTLS{Insecure: true},
		WebSocket: moq.WebSocketConfig{Enabled: ptr(false), Delay: ptr(time.Duration(0))},
	})
	if err != nil {
		t.Fatal(err)
	}
	_ = client.Close()

	if _, err := moq.Dial(ctx, url, moq.ClientConfig{WebSocket: moq.WebSocketConfig{Delay: ptr(-time.Millisecond)}}); err == nil {
		t.Fatal("negative websocket delay dialed")
	}
}

// Every connection knob reaches the native client and the dial still connects,
// and a negative duration fails Dial instead of wrapping into an enormous one.
func TestDialConnectionOptions(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), reconnectTimeout)
	defer cancel()

	r := startRelay(t, ctx, "127.0.0.1:0")
	defer r.server.Close()

	if len(moq.Versions()) == 0 {
		t.Fatal("no versions offered")
	}

	url := "https://" + r.addr
	client, err := moq.Dial(ctx, url, moq.ClientConfig{
		TLS:             moq.ClientTLS{Insecure: true, HostName: "localhost"},
		Versions:        moq.Versions(),
		Timeout:         ptr(5 * time.Second),
		Race:            ptr(time.Duration(0)),
		ResolutionDelay: ptr(time.Duration(0)),
		QUIC: moq.QUICConfig{
			IdleTimeout:       ptr(5 * time.Second),
			KeepAlive:         ptr(time.Duration(0)),
			GSO:               ptr(false),
			MTUDiscovery:      ptr(true),
			CongestionControl: ptr(moq.CongestionControlLoss),
		},
		WebSocket: moq.WebSocketConfig{Enabled: ptr(false)},
	})
	if err != nil {
		t.Fatal(err)
	}
	_ = client.Close()

	if _, err := moq.Dial(ctx, url, moq.ClientConfig{QUIC: moq.QUICConfig{IdleTimeout: ptr(-time.Millisecond)}}); err == nil {
		t.Fatal("negative idle timeout dialed")
	}
}

// A public wrapper owns its first dial through Session and observes repeated failures.
func TestFirstDialState(t *testing.T) {
	silent, err := net.ListenPacket("udp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer silent.Close()
	ctx, cancel := context.WithTimeout(context.Background(), reconnectTimeout)
	defer cancel()
	client, err := moq.Dial(ctx, "https://"+silent.LocalAddr().String(), moq.ClientConfig{
		Bind:      "127.0.0.1:0",
		Timeout:   ptr(100 * time.Millisecond),
		TLS:       moq.ClientTLS{Insecure: true},
		WebSocket: moq.WebSocketConfig{Enabled: ptr(false)},
		Backoff:   moq.Backoff{Initial: 50 * time.Millisecond, Max: 50 * time.Millisecond, Timeout: moq.RetryForever},
	})
	if err != nil {
		t.Fatal(err)
	}
	session := client.Session()
	defer session.Cancel(0)
	state := session.State()
	if state.Phase != moq.ConnectionPhaseConnecting || state.Connects != 0 {
		t.Fatalf("initial state: %+v", state)
	}
	for i := 0; i < 2; i++ {
		state, err = session.StateChanged(ctx, state.Revision)
		if err != nil {
			t.Fatal(err)
		}
		if state.Failure == nil || state.Connects != 0 || state.Version != nil {
			t.Fatalf("failed dial state: %+v", state)
		}
	}
	// A canceled waiter must leave both the session and another observer live.
	canceled, stop := context.WithCancel(ctx)
	stop()
	if _, err := session.StateChanged(canceled, state.Revision); !errors.Is(err, context.Canceled) {
		t.Fatalf("canceled waiter: %v", err)
	}
	if _, err := session.StateChanged(ctx, state.Revision); err != nil {
		t.Fatalf("independent waiter: %v", err)
	}
}
