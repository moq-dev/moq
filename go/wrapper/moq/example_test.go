package moq_test

import (
	"context"
	"fmt"
	"log"

	"github.com/moq-dev/moq-go/moq"
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

	announced, err := client.Announced("demos/")
	if err != nil {
		log.Fatal(err)
	}
	defer announced.Cancel()

	for ann, err := range announced.All(ctx) {
		if err != nil {
			if moq.IsShutdown(err) {
				break
			}
			log.Fatal(err)
		}
		fmt.Println("broadcast:", ann.Path())
	}
}

// Publish a media track to a relay.
func ExampleClient_Publish() {
	ctx := context.Background()

	client, err := moq.Dial(ctx, "https://relay.example.com")
	if err != nil {
		log.Fatal(err)
	}
	defer client.Close()

	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		log.Fatal(err)
	}
	defer broadcast.Finish()

	media, err := broadcast.PublishMedia("opus", opusHead())
	if err != nil {
		log.Fatal(err)
	}

	announce, err := client.Announce("me/mic", broadcast)
	if err != nil {
		log.Fatal(err)
	}
	// The broadcast stays announced until this handle goes away.
	defer announce.Unannounce()

	if err := media.WriteFrame(moq.Frame{Payload: []byte("opus frame")}); err != nil {
		log.Fatal(err)
	}
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
	fmt.Println("rtt:", stats.RttUs)
}

// Publish a video track with catalog hints known before the first keyframe.
func ExampleBroadcastProducer_PublishMedia_videoHint() {
	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		log.Fatal(err)
	}
	defer broadcast.Finish()

	media, err := broadcast.PublishMedia("avc3", nil, moq.WithVideoHint(moq.VideoHint{}))
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
func ExampleServer_Requests() {
	ctx := context.Background()

	server, err := moq.Listen(ctx, "127.0.0.1:4443", moq.WithTLSGenerate("localhost"))
	if err != nil {
		log.Fatal(err)
	}
	defer server.Close()

	for req, err := range server.Requests(ctx) {
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

		session, err := req.Accept(ctx)
		if err != nil {
			continue
		}
		// Hold the session to keep the connection alive.
		go func() { _ = session.Closed(ctx) }()
	}
}
