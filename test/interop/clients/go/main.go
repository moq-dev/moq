// Cross-language interop client for the interop test, built against the workspace
// go/wrapper module.
//
// publish reads raw Annex-B H.264 from stdin (e.g. piped from ffmpeg) and feeds
// it to a streaming importer, which infers frame boundaries. Alongside it, a
// synthetic tone is encoded through libopus so the matrix exercises the FFI
// audio path, not only the video one. subscribe connects, finds the video track
// in the catalog, and exits 0 as soon as any non-empty frame arrives (exit 1 on
// timeout or no data).
//
//	ffmpeg ... -f h264 - | go-interop publish --url http://localhost:4443 --broadcast b.hang
//	go-interop subscribe --url http://localhost:4443 --broadcast b.hang --timeout 20
//
// --auth-version offers only that version, one that carries AUTH: the client waits
// for the grant the relay sends and prints it as an `auth granted` line before it
// starts.
package main

import (
	"context"
	"encoding/binary"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"math"
	"os"
	"time"

	"moq.dev/moq"
	moqaudio "moq.dev/moq/audio"
	moqmedia "moq.dev/moq/media"
)

const readChunk = 64 * 1024

// media.TrackConsumer max delay: how much reordering the jitter buffer tolerates.
const maxDelay = time.Second

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
func publishTone(ctx context.Context, audio *moqaudio.Producer) {
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
		if err := audio.Write(moqaudio.Frame{Timestamp: time.Duration(timestampUs) * time.Microsecond, Data: data}); err != nil {
			fmt.Fprintf(os.Stderr, "tone: %v\n", err)
			return
		}
		phase += audioBatchSamples
		timestampUs += audioBatchUs
	}
}

// dial connects to url. With an authVersion, it offers only that version and waits
// for the relay's grant, printing it in the Rust client's `auth granted` shape.
func dial(ctx context.Context, url, authVersion string, timeout time.Duration) (*moq.Client, error) {
	opts := []moq.ClientOption{moq.WithTLSVerify(false)}
	if authVersion != "" {
		opts = append(opts, moq.WithVersions(authVersion))
	}
	client, err := moq.Dial(ctx, url, opts...)
	if err != nil || authVersion == "" {
		return client, err
	}

	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()
	auth := client.Session().Auth()
	grant := auth.Grant()
	for grant == nil {
		if grant, err = auth.GrantChanged(ctx); err != nil {
			client.Close()
			return nil, fmt.Errorf("grant: %w", err)
		}
	}
	fmt.Fprintf(os.Stderr, "auth granted publish=%s subscribe=%s\n", patterns(grant.Publish), patterns(grant.Subscribe))
	return client, nil
}

// patterns renders a grant's patterns as a JSON list, the shape the Rust client logs.
func patterns(list []string) string {
	if list == nil {
		list = []string{}
	}
	out, _ := json.Marshal(list)
	return string(out)
}

func publish(ctx context.Context, url, broadcast, authVersion string, timeout time.Duration) error {
	client, err := dial(ctx, url, authVersion, timeout)
	if err != nil {
		return err
	}
	defer client.Close()

	// Hold the producer for the lifetime of the publish loop; Close unpublishes.
	producer, err := client.CreateBroadcast(broadcast)
	if err != nil {
		return err
	}
	defer producer.Close()

	catalog, err := moqmedia.NewCatalogProducer(producer)
	if err != nil {
		return err
	}
	media, err := moqmedia.NewVideoTrackStreamProducer(producer, moqmedia.Named{}, moqmedia.VideoInit{Format: moqmedia.VideoFormatAvc3, Data: nil})
	if err != nil {
		return err
	}
	audio, err := moqaudio.NewProducer(catalog, moqaudio.EncoderInput{
		Format:     moq.AudioSampleFormatF32,
		SampleRate: audioRate,
		Channels:   1,
	}, moqaudio.EncoderOutput{
		Codec:         moq.OpusAudioCodec(),
		Track:         func() *string { track := audioTrack; return &track }(),
		FrameDuration: audioFrameDurationUs * time.Microsecond,
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
	// producer. Runs before the deferred producer.Close, and is a no-op once
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
func catalogWithVideo(ctx context.Context, consumer *moq.BroadcastConsumer) (*moqmedia.Catalog, error) {
	catalogs, err := moqmedia.NewCatalogConsumer(ctx, consumer)
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

func subscribe(ctx context.Context, url, broadcast, authVersion string, timeout time.Duration) error {
	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()

	client, err := dial(ctx, url, authVersion, timeout)
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

	var id string
	var video moqmedia.Video
	for id, video = range catalog.Video {
		break
	}

	media, err := moqmedia.NewTrackConsumer(ctx, consumer, moqmedia.TrackConfig{Rendition: moqmedia.VideoRendition(id, video), Subscription: &moq.Subscription{MaxDelay: maxDelay}})
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
		return errors.New("usage: go-interop publish|subscribe --url URL --broadcast PATH [--timeout SECONDS] [--auth-version V]")
	}
	role := os.Args[1]

	flags := flag.NewFlagSet(role, flag.ExitOnError)
	url := flags.String("url", "", "relay URL")
	broadcast := flags.String("broadcast", "", "broadcast path")
	timeout := flags.Float64("timeout", 20, "seconds to wait for data")
	authVersion := flags.String("auth-version", "", "offer only this version, which carries AUTH, and print the grant")
	if err := flags.Parse(os.Args[2:]); err != nil {
		return err
	}
	if *url == "" || *broadcast == "" {
		return errors.New("--url and --broadcast are required")
	}

	// Warnings and errors from the Rust layer, such as a publication outside the grant.
	if err := moq.LogLevel("warn"); err != nil {
		return err
	}
	wait := time.Duration(*timeout * float64(time.Second))
	ctx := context.Background()
	switch role {
	case "publish":
		return publish(ctx, *url, *broadcast, *authVersion, wait)
	case "subscribe":
		return subscribe(ctx, *url, *broadcast, *authVersion, wait)
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
