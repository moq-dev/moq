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

	if err := client.Announce("me/mic", broadcast); err != nil {
		log.Fatal(err)
	}
	if err := media.WriteFrame([]byte("opus frame"), 0); err != nil {
		log.Fatal(err)
	}
}

// Run a server with a self-signed certificate, accepting every session.
func ExampleListen() {
	ctx := context.Background()

	server, err := moq.Listen(ctx, "127.0.0.1:4443", moq.WithTLSGenerate("localhost"))
	if err != nil {
		log.Fatal(err)
	}
	defer server.Close()

	err = server.Serve(ctx, func(req *moq.Request) (bool, error) {
		// Reject anything that didn't arrive over QUIC.
		return req.Transport() == moq.TransportQUIC, nil
	})
	if err != nil && !moq.IsShutdown(err) {
		log.Fatal(err)
	}
}
