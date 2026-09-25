package moq_test

import (
	"context"
	"fmt"
	"sync"
	"testing"
	"time"

	"moq.dev/moq"
)

// reconnectTimeout bounds the relay-restart test; its backoff floor is 50ms, so
// even a slow CI machine reconnects well inside this.
const reconnectTimeout = 30 * time.Second

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
		for req, err := range server.Requests(ctx) {
			if err != nil {
				return
			}
			session, err := req.Accept(ctx)
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

	client, err := moq.Dial(ctx, url,
		moq.WithTLSVerify(false),
		moq.WithBackoff(moq.Backoff{
			Initial: 50 * time.Millisecond,
			Max:     time.Second,
			Timeout: moq.RetryForever,
		}),
	)
	if err != nil {
		t.Fatal(err)
	}
	return client
}

// waitEpoch blocks until the session reports at least want connections, proving
// the worker redialed rather than just reusing the transport.
func waitEpoch(t *testing.T, ctx context.Context, session *moq.Session, want uint64) {
	t.Helper()

	for session.Epoch() < want {
		select {
		case <-ctx.Done():
			t.Fatalf("epoch = %d, want >= %d", session.Epoch(), want)
		case <-time.After(10 * time.Millisecond):
		}
	}
}

// awaitAnnouncement drains announcements until an active one names path.
func awaitAnnouncement(t *testing.T, ctx context.Context, announced *moq.AnnounceConsumer, path string) {
	t.Helper()

	for {
		ann, err := announced.Next(ctx)
		if err != nil {
			t.Fatalf("waiting for %q: %v", path, err)
		}
		if ann == nil {
			t.Fatalf("announcement stream ended before %q", path)
		}
		if ann.Active() && ann.Prefix() == path {
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

// A Go worker survives a relay restart the way a libmoq worker does: the session
// redials with backoff, the publisher re-announces its broadcast to the fresh
// relay, and the subscriber's subscription receives frames again. The epoch
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

	broadcast, err := publisher.CreateBroadcast("live")
	if err != nil {
		t.Fatal(err)
	}
	defer broadcast.Close()

	track, err := broadcast.PublishTrack("data", nil)
	if err != nil {
		t.Fatal(err)
	}
	defer track.Finish()
	if err := broadcast.Announce(moq.Route{}); err != nil {
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
			_ = track.WriteFrame(moq.Frame{Payload: fmt.Appendf(nil, "frame-%d", i)})
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

	// Drop every session and the listener, then bind a replacement on the same
	// port: a deploy, with an origin the replacement has never seen.
	current = current.restart(t, ctx)
	defer func() { _ = current.server.Close() }()

	// Both workers observe the reconnect and advance their epoch past the first.
	waitEpoch(t, ctx, publisher.Session(), 2)
	waitEpoch(t, ctx, subscriber.Session(), 2)

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
	client, err := moq.Dial(ctx, url,
		moq.WithTLSVerify(false),
		moq.WithWebSocketEnabled(false),
		moq.WithWebSocketDelay(0),
	)
	if err != nil {
		t.Fatal(err)
	}
	_ = client.Close()

	if _, err := moq.Dial(ctx, url, moq.WithWebSocketDelay(-time.Millisecond)); err == nil {
		t.Fatal("negative websocket delay dialed")
	}
}
