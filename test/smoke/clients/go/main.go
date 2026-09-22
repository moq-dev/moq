// Cross-language interop client for the smoke test, built against the workspace
// go/wrapper module.
//
// publish reads raw Annex-B H.264 from stdin (e.g. piped from ffmpeg) and feeds
// it to a streaming importer, which infers frame boundaries. Alongside it, a
// synthetic tone is encoded through libopus so the matrix exercises the FFI
// audio path, not only the video one. subscribe connects, finds the video track
// in the catalog, and exits 0 as soon as any non-empty frame arrives (exit 1 on
// timeout or no data).
//
//	ffmpeg ... -f h264 - | go-smoke publish --url http://localhost:4443 --broadcast b.hang
//	go-smoke subscribe --url http://localhost:4443 --broadcast b.hang --timeout 20
package main

import (
	"context"
	"encoding/binary"
	"errors"
	"flag"
	"fmt"
	"io"
	"math"
	"os"
	"time"

	"moq.dev/moq"
)

const readChunk = 64 * 1024

// SubscribeMedia max age: how much reordering the jitter buffer tolerates.
const maxAgeUs = 1_000_000

// Synthetic audio: a 48 kHz mono tone, encoded as Opus.
const (
	audioTrack  = "tone"
	audioRate   = 48_000
	audioToneHz = 440.0
	// A non-default frame duration, and the shortest Opus offers. 20 ms would
	// pass even if the microsecond field were truncated to milliseconds
	// somewhere.
	audioFrameDurationUs = 2_500
	// Written in 20 ms batches, so each write spans eight encoded Opus frames.
	audioBatchUs      = 20_000
	audioBatchSamples = audioRate * audioBatchUs / 1_000_000
)

// publishTone feeds the encoder a real-time tone until ctx is cancelled.
func publishTone(ctx context.Context, audio *moq.AudioProducer) {
	ticker := time.NewTicker(audioBatchUs * time.Microsecond)
	defer ticker.Stop()

	data := make([]byte, audioBatchSamples*4)
	timestampUs := uint64(0)
	phase := 0
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		}
		for i := 0; i < audioBatchSamples; i++ {
			sample := math.Sin(2 * math.Pi * audioToneHz * float64(phase+i) / audioRate)
			binary.LittleEndian.PutUint32(data[i*4:], math.Float32bits(float32(sample)))
		}
		if err := audio.Write(moq.AudioFrame{TimestampUs: timestampUs, Data: data}); err != nil {
			fmt.Fprintf(os.Stderr, "tone: %v\n", err)
			return
		}
		phase += audioBatchSamples
		timestampUs += audioBatchUs
	}
}

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

	media, err := producer.PublishVideoStream(moq.VideoFormatAvc3)
	if err != nil {
		return err
	}
	audio, err := producer.EncodeAudio(audioTrack, moq.AudioEncoderInput{
		Format:     moq.AudioSampleFormatF32,
		SampleRate: audioRate,
		Channels:   1,
	}, moq.AudioEncoderOutput{
		Codec:           moq.OpusAudioCodec(),
		FrameDurationUs: audioFrameDurationUs,
	}, nil)
	if err != nil {
		return err
	}
	if err := producer.Announce(moq.Route{}); err != nil {
		return err
	}
	fmt.Printf("publishing %q (Annex-B H.264 from stdin + a %.0f Hz tone) to %s\n", broadcast, audioToneHz, url)

	toneCtx, stopTone := context.WithCancel(ctx)
	toneDone := make(chan struct{})
	go func() {
		defer close(toneDone)
		publishTone(toneCtx, audio)
	}()
	// Let the tone unwind before any Finish, so no write races a finished
	// producer. Runs before the deferred producer.Finish, and is a no-op once
	// the happy path below has already stopped and joined it.
	defer func() {
		stopTone()
		<-toneDone
	}()

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
	stopTone()
	<-toneDone
	if err := audio.Finish(); err != nil {
		return err
	}
	return media.Finish()
}

// The catalog is a live track. A lazy publisher (e.g. the browser, which only
// encodes on demand) may announce video in a later update rather than the first
// snapshot, so wait for a catalog that actually has a video track.
func catalogWithVideo(ctx context.Context, consumer *moq.BroadcastConsumer) (*moq.Catalog, error) {
	catalogs, err := consumer.SubscribeCatalog(ctx)
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

	media, err := consumer.SubscribeMedia(ctx, name, video.Container, &moq.Subscription{MaxAgeUs: maxAgeUs})
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
