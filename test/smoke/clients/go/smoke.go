// Cross-language interop client for the smoke test, built against the ergonomic
// moq.dev/moq wrapper (package moq: Dial, CreateBroadcast,
// PublishVideoStream, SubscribeMedia, range-over-func frame iterators). The raw
// uniffi-bindgen-go surface lives in moq.dev/moq-ffi; this client
// exercises the idiomatic wrapper a real Go user would reach for.
//
// publish reads raw Annex-B H.264 from stdin (e.g. piped from ffmpeg) and feeds
// it to a streaming importer, which infers frame boundaries. subscribe connects,
// finds the video track in the catalog, and exits 0 as soon as any non-empty
// frame arrives (exit 1 on timeout / no data). Usage:
//
//	ffmpeg ... -f h264 - | go-smoke publish --url http://127.0.0.1:4443 --broadcast b.hang
//	go-smoke subscribe --url http://127.0.0.1:4443 --broadcast b.hang --timeout 20
package main

import (
	"context"
	"flag"
	"fmt"
	"io"
	"os"
	"time"

	"moq.dev/moq"
)

const readChunk = 64 * 1024

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
	// The relay uses a self-signed cert, so verification is off. With no origin
	// options, Dial shares one origin for publish + consume.
	client, err := moq.Dial(context.Background(), url, moq.WithTLSVerify(false))
	if err != nil {
		return err
	}
	defer client.Close()

	producer, err := client.CreateBroadcast(broadcast)
	if err != nil {
		return err
	}
	defer producer.Finish()

	// avc3: a self-describing Annex-B H.264 stream the importer can frame on its
	// own. PublishVideoStream feeds the raw byte stream; whole frames are emitted
	// as they complete.
	media, err := producer.PublishVideoStream(moq.VideoFormatAvc3)
	if err != nil {
		return err
	}
	defer media.Finish()

	// CreateBroadcast only registers the path locally; Announce advertises it to
	// the relay, once its tracks exist.
	if err := producer.Announce(moq.Route{}); err != nil {
		return err
	}

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
	// The whole subscribe must complete within the timeout; a cancelled context
	// aborts any in-flight wrapper call and unblocks the frame iterator.
	ctx, cancel := context.WithTimeout(context.Background(), time.Duration(timeoutS*float64(time.Second)))
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

	bc, err := announced.Available(ctx)
	if err != nil {
		return err
	}

	name, video, err := videoTrack(ctx, bc)
	if err != nil {
		return err
	}

	media, err := bc.SubscribeMedia(ctx, name, video.Container, nil)
	if err != nil {
		return err
	}
	defer media.Cancel()

	for frame, err := range media.Frames(ctx) {
		if err != nil {
			return err
		}
		if len(frame.Payload) > 0 {
			fmt.Fprintf(os.Stderr, "received %d bytes from %q\n", len(frame.Payload), broadcast)
			return nil
		}
	}
	return fmt.Errorf("no frame data received")
}

// videoTrack waits for a catalog update that actually carries a video track. A
// lazy publisher (e.g. the browser, which only encodes on demand) may announce
// video in a later update, not the first snapshot.
func videoTrack(ctx context.Context, bc *moq.BroadcastConsumer) (string, moq.Video, error) {
	cat, err := bc.SubscribeCatalog(ctx)
	if err != nil {
		return "", moq.Video{}, err
	}
	defer cat.Cancel()

	for catalog, err := range cat.Updates(ctx) {
		if err != nil {
			return "", moq.Video{}, err
		}
		for name, video := range catalog.Video {
			return name, video, nil
		}
	}
	return "", moq.Video{}, fmt.Errorf("catalog stream ended without a video track")
}
