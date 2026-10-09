package moq_test

import (
	"context"
	"fmt"
	"log"
	"time"

	"moq.dev/moq"
	moqmedia "moq.dev/moq/media"
)

// Subscribe to a broadcast and print its catalog. These examples have no Output
// line, so they are compiled (API-checked) but not executed.
func ExampleClient_Announced() {
	ctx := context.Background()

	client, err := moq.Dial(ctx, "https://relay.example.com")
	if err != nil {
		log.Fatal(err)
	}
	defer client.Close()

	announced, err := client.Announced(moq.AnnounceOptions{Prefix: "demos/"})
	if err != nil {
		log.Fatal(err)
	}
	defer announced.Cancel()

	for event, err := range announced.All(ctx) {
		if err != nil {
			if moq.IsShutdown(err) {
				break
			}
			log.Fatal(err)
		}
		if event, ok := event.(moq.AnnounceEventStart); ok {
			fmt.Println("broadcast:", event.Announce.Prefix)
		}
	}
}

// Publish a media track to a relay.
func ExampleClient_CreateBroadcast() {
	ctx := context.Background()

	client, err := moq.Dial(ctx, "https://relay.example.com")
	if err != nil {
		log.Fatal(err)
	}
	defer client.Close()

	broadcast, err := client.CreateBroadcast("me/mic")
	if err != nil {
		log.Fatal(err)
	}

	media, err := moqmedia.NewAudioTrackProducer(broadcast, moqmedia.Named{}, moqmedia.AudioInit{Format: moqmedia.AudioFormatOpus, Data: opusHead()})
	if err != nil {
		log.Fatal(err)
	}
	_ = broadcast.Announce(moq.Route{})

	pts := 20 * time.Millisecond
	if err := media.WriteFrame(moq.Frame{Payload: []byte("opus frame"), Timestamp: &pts}); err != nil {
		log.Fatal(err)
	}

	// Finish before closing: Close drains, and a live track never drains.
	_ = media.Finish()
	// Closing ends the broadcast for good.
	broadcast.Close()
}

// Connect with pinned TLS material and read a stats snapshot.
func ExampleClient_Session_stats() {
	ctx := context.Background()

	client, err := moq.Dial(ctx, "https://relay.example.com",
		moq.WithTLSRoots("/etc/ssl/custom-ca.pem"),
		moq.WithTLSSystemRoots(true),
		moq.WithTLSFingerprints("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"),
	)
	if err != nil {
		log.Fatal(err)
	}
	defer client.Close()

	stats := client.Session().Stats()
	fmt.Println("rtt:", stats.RTT)
}

// Publish a video track with catalog hints known before the first keyframe.
func Example_mediaVideoHint() {
	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		log.Fatal(err)
	}
	defer broadcast.Close()

	media, err := moqmedia.NewVideoTrackProducer(broadcast, moqmedia.Named{}, moqmedia.VideoInit{Format: moqmedia.VideoFormatAvc3, Data: nil, Hint: &moqmedia.VideoHint{}})
	if err != nil {
		log.Fatal(err)
	}
	defer media.Finish()
}

// Run a server with a self-signed certificate, accepting every session.
func ExampleListen() {
	ctx := context.Background()

	server, err := moq.Listen(ctx, "127.0.0.1:4443", moq.WithTLSGenerate("localhost"))
	if err != nil {
		log.Fatal(err)
	}
	defer server.Close()

	if err := server.Serve(ctx); err != nil && !moq.IsShutdown(err) {
		log.Fatal(err)
	}
}

// Drive the accept loop directly to decide which sessions to admit.
func ExampleServer_All() {
	ctx := context.Background()

	server, err := moq.Listen(ctx, "127.0.0.1:4443", moq.WithTLSGenerate("localhost"))
	if err != nil {
		log.Fatal(err)
	}
	defer server.Close()

	for req, err := range server.All(ctx) {
		if err != nil {
			if moq.IsShutdown(err) {
				break
			}
			log.Fatal(err)
		}

		// Reject anything that didn't arrive over QUIC.
		if req.Transport() != moq.TransportQUIC {
			_ = req.Reject(ctx, 403)
			continue
		}

		session, err := req.Accept(ctx, nil, nil)
		if err != nil {
			continue
		}
		// Hold the session to keep the connection alive.
		go func() { _ = session.Closed(ctx) }()
	}
}
