// Cross-language interop client for the smoke test, built against the workspace
// go/wrapper module.
//
// publish reads raw Annex-B H.264 from stdin (e.g. piped from ffmpeg) and feeds
// it to a streaming importer, which infers frame boundaries. subscribe connects,
// finds the video track in the catalog, and exits 0 as soon as any non-empty
// frame arrives (exit 1 on timeout or no data).
//
//	ffmpeg ... -f h264 - | go-smoke publish --url http://localhost:4443 --broadcast b.hang
//	go-smoke subscribe --url http://localhost:4443 --broadcast b.hang --timeout 20
package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"time"

	"github.com/moq-dev/moq-go/moq"
)

const readChunk = 64 * 1024

// SubscribeMedia congestion-control / lookahead window.
const latencyMaxMs = 1_000

func publish(ctx context.Context, url, broadcast string) error {
	client, err := moq.Dial(ctx, url, moq.WithTLSVerify(false))
	if err != nil {
		return err
	}
	defer client.Close()

	// Hold the producer for the lifetime of the publish loop; Finish unpublishes.
	producer, err := client.CreateBroadcast(broadcast)
	if err != nil {
		return err
	}
	defer producer.Finish()

	media, err := producer.PublishMediaStream("avc3")
	if err != nil {
		return err
	}
	fmt.Printf("publishing %q (Annex-B H.264 from stdin) to %s\n", broadcast, url)

	// os.Stdin.Read returns as soon as any bytes are available, so ffmpeg's
	// real-time output is forwarded rather than batched into full chunks.
	buf := make([]byte, readChunk)
	for {
		n, err := os.Stdin.Read(buf)
		if n > 0 {
			if err := media.Write(buf[:n]); err != nil {
				return err
			}
		}
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			return err
		}
	}
	return media.Finish()
}

// The catalog is a live track. A lazy publisher (e.g. the browser, which only
// encodes on demand) may announce video in a later update rather than the first
// snapshot, so wait for a catalog that actually has a video track.
func catalogWithVideo(ctx context.Context, consumer *moq.BroadcastConsumer) (*moq.Catalog, error) {
	catalogs, err := consumer.SubscribeCatalog()
	if err != nil {
		return nil, err
	}
	defer catalogs.Cancel()

	for {
		catalog, err := catalogs.Next(ctx)
		if err != nil {
			return nil, err
		}
		if catalog == nil {
			return nil, errors.New("catalog stream ended without a video track")
		}
		if len(catalog.Video) > 0 {
			return catalog, nil
		}
	}
}

func subscribe(ctx context.Context, url, broadcast string, timeout time.Duration) error {
	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()

	client, err := moq.Dial(ctx, url, moq.WithTLSVerify(false))
	if err != nil {
		return err
	}
	defer client.Close()

	announced, err := client.AnnouncedBroadcast(broadcast)
	if err != nil {
		return err
	}
	defer announced.Cancel()

	consumer, err := announced.Available(ctx)
	if err != nil {
		return err
	}

	catalog, err := catalogWithVideo(ctx, consumer)
	if err != nil {
		return err
	}

	var name string
	var video moq.Video
	for name, video = range catalog.Video {
		break
	}

	media, err := consumer.SubscribeMedia(name, video.Container, &moq.Subscription{LatencyMaxMs: latencyMaxMs})
	if err != nil {
		return err
	}
	defer media.Cancel()

	total := 0
	for total == 0 {
		frame, err := media.Next(ctx)
		if err != nil {
			return err
		}
		if frame == nil {
			break
		}
		total += len(frame.Payload)
	}
	if total == 0 {
		return errors.New("no frame data received")
	}
	fmt.Printf("received %d bytes from %q\n", total, broadcast)
	return nil
}

func run() error {
	if len(os.Args) < 2 {
		return errors.New("usage: go-smoke publish|subscribe --url URL --broadcast PATH [--timeout SECONDS]")
	}
	role := os.Args[1]

	flags := flag.NewFlagSet(role, flag.ExitOnError)
	url := flags.String("url", "", "relay URL")
	broadcast := flags.String("broadcast", "", "broadcast path")
	timeout := flags.Float64("timeout", 20, "seconds to wait for data")
	if err := flags.Parse(os.Args[2:]); err != nil {
		return err
	}
	if *url == "" || *broadcast == "" {
		return errors.New("--url and --broadcast are required")
	}

	ctx := context.Background()
	switch role {
	case "publish":
		return publish(ctx, *url, *broadcast)
	case "subscribe":
		return subscribe(ctx, *url, *broadcast, time.Duration(*timeout*float64(time.Second)))
	default:
		return fmt.Errorf("unknown role %q", role)
	}
}

func main() {
	if err := run(); err != nil {
		fmt.Fprintf(os.Stderr, "error: %v\n", err)
		os.Exit(1)
	}
}
