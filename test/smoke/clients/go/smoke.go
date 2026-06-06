// Cross-language interop client for the smoke test, built against the published
// github.com/moq-dev/moq-go module (the raw uniffi-bindgen-go surface).
//
// publish:   read raw Annex-B H.264 from stdin (e.g. piped from ffmpeg) and feed
//            it to a streaming importer, which infers frame boundaries.
// subscribe: connect, find the video track in the catalog, and exit 0 as soon as
//            any non-empty frame arrives (exit 1 on timeout / no data).
//
//	ffmpeg ... -f h264 - | go-smoke publish   --url http://127.0.0.1:4443 --broadcast b.hang
//	                       go-smoke subscribe --url http://127.0.0.1:4443 --broadcast b.hang --timeout 20
package main

import (
	"flag"
	"fmt"
	"io"
	"os"
	"time"

	"github.com/moq-dev/moq-go/moq"
)

const (
	readChunk    = 64 * 1024
	maxLatencyMs = 1000 // subscribe_media congestion-control / lookahead window
)

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: go-smoke publish|subscribe --url U --broadcast B [--timeout S]")
		os.Exit(2)
	}
	role := os.Args[1]

	fs := flag.NewFlagSet(role, flag.ExitOnError)
	url := fs.String("url", "", "MoQ server URL")
	broadcast := fs.String("broadcast", "", "broadcast name")
	timeout := fs.Float64("timeout", 20, "subscribe timeout in seconds")
	_ = fs.Parse(os.Args[2:])

	if *url == "" || *broadcast == "" {
		fmt.Fprintln(os.Stderr, "error: --url and --broadcast are required")
		os.Exit(2)
	}

	var err error
	switch role {
	case "publish":
		err = publish(*url, *broadcast)
	case "subscribe":
		err = subscribe(*url, *broadcast, *timeout)
	default:
		fmt.Fprintf(os.Stderr, "unknown role: %s\n", role)
		os.Exit(2)
	}

	if err != nil {
		fmt.Fprintf(os.Stderr, "error: %v\n", err)
		os.Exit(1)
	}
}

func publish(url, broadcast string) error {
	origin := moq.NewMoqOriginProducer()
	defer origin.Destroy()

	producer, err := moq.NewMoqBroadcastProducer()
	if err != nil {
		return err
	}
	defer producer.Destroy()

	// avc3: a self-describing Annex-B H.264 stream the importer can frame on its own.
	media, err := producer.PublishMediaStream("avc3")
	if err != nil {
		return err
	}
	defer media.Destroy()

	if err := origin.Publish(broadcast, producer); err != nil {
		return err
	}

	client := moq.NewMoqClient()
	defer client.Destroy()
	client.SetTlsDisableVerify(true)
	client.SetPublish(&origin)

	session, err := client.Connect(url)
	if err != nil {
		return err
	}
	defer session.Destroy()

	fmt.Fprintf(os.Stderr, "publishing %q (Annex-B H.264 from stdin) to %s\n", broadcast, url)

	buf := make([]byte, readChunk)
	for {
		n, rerr := os.Stdin.Read(buf)
		if n > 0 {
			if werr := media.Write(buf[:n]); werr != nil {
				return werr
			}
		}
		if rerr == io.EOF {
			break
		}
		if rerr != nil {
			return rerr
		}
	}
	return media.Finish()
}

func subscribe(url, broadcast string, timeoutS float64) error {
	done := make(chan error, 1)
	go func() { done <- subscribeInner(url, broadcast) }()

	select {
	case err := <-done:
		return err
	case <-time.After(time.Duration(timeoutS * float64(time.Second))):
		// The FFI calls below block; the process exit tears down the goroutine.
		return fmt.Errorf("timed out waiting for data")
	}
}

func subscribeInner(url, broadcast string) error {
	origin := moq.NewMoqOriginProducer()
	defer origin.Destroy()

	client := moq.NewMoqClient()
	defer client.Destroy()
	client.SetTlsDisableVerify(true)
	client.SetConsume(&origin)

	session, err := client.Connect(url)
	if err != nil {
		return err
	}
	defer session.Destroy()

	consumer := origin.Consume()
	defer consumer.Destroy()

	announced, err := consumer.AnnouncedBroadcast(broadcast)
	if err != nil {
		return err
	}
	defer announced.Destroy()

	bc, err := announced.Available()
	if err != nil {
		return err
	}
	defer bc.Destroy()

	name, video, err := videoTrack(bc)
	if err != nil {
		return err
	}

	media, err := bc.SubscribeMedia(name, video.Container, maxLatencyMs)
	if err != nil {
		return err
	}
	defer media.Destroy()

	total := 0
	for {
		frame, err := media.Next()
		if err != nil {
			return err
		}
		if frame == nil {
			break
		}
		total += len(frame.Payload)
		if total > 0 {
			fmt.Fprintf(os.Stderr, "received %d bytes from %q\n", total, broadcast)
			return nil
		}
	}
	return fmt.Errorf("no frame data received")
}

// videoTrack waits for a catalog update that actually carries a video track. A
// lazy publisher (e.g. the browser, which only encodes on demand) may announce
// video in a later update, not the first snapshot.
func videoTrack(bc *moq.MoqBroadcastConsumer) (string, moq.MoqVideo, error) {
	cat, err := bc.SubscribeCatalog()
	if err != nil {
		return "", moq.MoqVideo{}, err
	}
	defer cat.Destroy()

	for {
		catalog, err := cat.Next()
		if err != nil {
			return "", moq.MoqVideo{}, err
		}
		if catalog == nil {
			return "", moq.MoqVideo{}, fmt.Errorf("catalog stream ended without a video track")
		}
		for name, video := range catalog.Video {
			return name, video, nil
		}
	}
}
