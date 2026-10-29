// Cross-language interop client for the interop test, built against the workspace
// go/wrapper module.
//
// publish reads raw Annex-B H.264 from stdin (e.g. piped from ffmpeg) and feeds
// it to a streaming importer, which infers frame boundaries. Alongside it, a
// tone per channel (--tone) is encoded through libopus so the matrix exercises
// the FFI audio path, not only the video one. subscribe connects, waits for a
// catalog with video and audio, and exits 0 once a non-empty video frame has
// arrived and --audio-ms of every audio rendition has been decoded to
// --audio-out.<codec> for tone.py to judge (exit 1 on timeout or no data).
//
//	ffmpeg ... -f h264 - | go-interop publish --url http://localhost:4443 --broadcast b.hang --tone 440,660
//	go-interop subscribe --url http://localhost:4443 --broadcast b.hang --timeout 20 --audio-out /tmp/audio --audio-ms 600
//
// --finish verifies a finite tail, natural media/catalog EOF, announcement END, and
// same-session health; --frames sets its exact size. --finish-ack selects publisher
// finish mode and names the FIFO releasing teardown after the real linger.
// --version offers only that version. --grant waits for the grant the relay sends
// over AUTH and prints it as an `auth granted` line before the client starts.
package main

import (
	"bufio"
	"context"
	"encoding/binary"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"math"
	"os"
	"strconv"
	"strings"
	"time"

	"moq.dev/moq"
	moqaudio "moq.dev/moq/audio"
	moqmedia "moq.dev/moq/media"
)

const readChunk = 64 * 1024

// How far a read may fall behind live before it skips. A gap fails the run, which a reader skipping
// behind live would cause itself: the matrix starts every subscriber at once, and a runtime starved
// by the others can fall a second behind. That is this reader's speed, not the delivery under test.
// A track's max-age caps it, so interop.sh's Rust publisher keeps its media at least this long.
const maxDelay = 5 * time.Second

// finishLinger is how long a finished publisher stays up for the late readers.
const finishLinger = 10 * time.Second

// durationPtr is d as the optional duration the wrapper's options take.
func durationPtr(d time.Duration) *time.Duration { return &d }

// Synthetic audio: a 48 kHz tone per channel at 0.5 amplitude, encoded as Opus.
const (
	audioTrack     = "tone"
	audioRate      = 48_000
	audioAmplitude = 0.5
	// A non-default frame duration, and the shortest Opus offers. 20 ms would
	// pass even if the microsecond field were truncated to milliseconds
	// somewhere.
	audioFrameDurationUs = 2_500
	// Written in 20 ms batches, so each write spans eight encoded Opus frames.
	audioBatchUs      = 20_000
	audioBatchSamples = audioRate * audioBatchUs / 1_000_000
)

// publishTone feeds the encoder a real-time tone per channel, interleaved, until ctx is cancelled.
func publishTone(ctx context.Context, audio *moqaudio.Producer, tones []float64) {
	ticker := time.NewTicker(audioBatchUs * time.Microsecond)
	defer ticker.Stop()

	data := make([]byte, audioBatchSamples*len(tones)*4)
	timestampUs := uint64(0)
	phase := 0
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		}
		for i := 0; i < audioBatchSamples; i++ {
			for c, hz := range tones {
				sample := audioAmplitude * math.Sin(2*math.Pi*hz*float64(phase+i)/audioRate)
				binary.LittleEndian.PutUint32(data[(i*len(tones)+c)*4:], math.Float32bits(float32(sample)))
			}
		}
		if err := audio.Write(moqaudio.Frame{Timestamp: time.Duration(timestampUs) * time.Microsecond, Data: data}); err != nil {
			fmt.Fprintf(os.Stderr, "tone: %v\n", err)
			return
		}
		phase += audioBatchSamples
		timestampUs += audioBatchUs
	}
}

// dialOptions are the session flags both roles share.
type dialOptions struct {
	// version, when set, is the only version offered.
	version string
	// grant waits for the relay's grant and prints it.
	grant bool
}

// dial connects to url, printing the relay's grant in the Rust client's
// `auth granted` shape when asked.
func dial(ctx context.Context, url string, opts dialOptions, timeout time.Duration) (*moq.Client, error) {
	config := moq.ClientConfig{TLS: moq.ClientTLS{Insecure: true}}
	if opts.version != "" {
		config.Versions = []string{opts.version}
	}
	client, err := moq.Dial(ctx, url, config)
	if err != nil || !opts.grant {
		return client, err
	}

	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()
	grants := client.Session().Grants()
	defer grants.Close()
	grant := grants.Grant()
	for grant == nil {
		if grant, err = grants.Changed(ctx); err != nil {
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

func publish(ctx context.Context, url, broadcast string, opts dialOptions, timeout time.Duration, tones []float64, finishAck string) error {
	client, err := dial(ctx, url, opts, timeout)
	if err != nil {
		return err
	}
	defer client.Close()

	// Hold the producer for the lifetime of the publish loop; Close unpublishes.
	publication, err := client.CreateBroadcast(broadcast)
	if err != nil {
		return err
	}
	producer := publication.Broadcast()
	defer producer.Close()

	catalog, err := moqmedia.NewCatalogProducer(producer)
	if err != nil {
		return err
	}
	media, err := moqmedia.NewVideoTrackStreamProducer(producer, moqmedia.Named{}, moqmedia.VideoInit{Format: moqmedia.VideoFormatAvc3, Data: nil})
	if err != nil {
		return err
	}
	audioTrackName := audioTrack
	audio, err := moqaudio.NewProducer(catalog, moqaudio.ProducerOptions{
		Input: moqaudio.EncoderInput{
			Format:     moq.AudioSampleFormatF32,
			SampleRate: audioRate,
			Channels:   uint32(len(tones)),
		},
		Output: moqaudio.EncoderOutput{
			Codec:         moq.OpusAudioCodec(),
			FrameDuration: audioFrameDurationUs * time.Microsecond,
		},
		Track: &audioTrackName,
	})
	if err != nil {
		return err
	}
	if err := publication.Announce(moq.AnnounceRoute{}); err != nil {
		return err
	}
	fmt.Printf("publishing %q (Annex-B H.264 from stdin + a %v Hz tone) to %s\n", broadcast, tones, url)

	toneCtx, stopTone := context.WithCancel(ctx)
	toneDone := make(chan struct{})
	go func() {
		defer close(toneDone)
		publishTone(toneCtx, audio, tones)
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
	if err := media.Finish(); err != nil {
		return err
	}
	if finishAck == "" {
		return nil
	}
	// Finishing the catalog ends the content; the broadcast stays up for the late readers
	// until Close takes it offline. ACK never controls the linger.
	if err := catalog.Finish(); err != nil {
		return err
	}
	fmt.Printf("finish ready linger_us=%d\n", finishLinger.Microseconds())
	select {
	case <-time.After(finishLinger):
	case <-ctx.Done():
		return ctx.Err()
	}
	if err := producer.Close(); err != nil {
		return err
	}
	healthPublication, err := client.CreateBroadcast(broadcast + ".health")
	if err != nil {
		return err
	}
	health := healthPublication.Broadcast()
	defer health.Close()
	live, err := health.PublishTrack("live", nil)
	if err != nil {
		return err
	}
	if err := live.WriteFrame(moq.Frame{Payload: []byte("session-alive")}); err != nil {
		return err
	}
	if err := live.Finish(); err != nil {
		return err
	}
	if err := healthPublication.Announce(moq.AnnounceRoute{}); err != nil {
		return err
	}
	ack, err := os.Open(finishAck)
	if err != nil {
		return err
	}
	defer ack.Close()
	command, err := bufio.NewReader(ack).ReadString('\n')
	if err != nil {
		return err
	}
	if command != "complete\n" {
		return errors.New("expected finish completion acknowledgement")
	}
	if client.Session().State().Connects != 1 {
		return errors.New("publisher reconnected during finish")
	}
	fmt.Println("finish acknowledged")
	return nil
}

// The catalog is a live track. A lazy publisher (e.g. the browser, which only
// encodes on demand) may announce a rendition in a later update rather than the
// first snapshot, so wait for a catalog that has both video and audio.
func catalogWithMedia(ctx context.Context, consumer *moq.BroadcastConsumer) (*moqmedia.Catalog, error) {
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
			return nil, errors.New("catalog stream ended without a video and an audio track")
		}
		if len(catalog.Video) > 0 && len(catalog.Audio) > 0 {
			return catalog, nil
		}
	}
}

// finishSubscribe retains each reader until its natural EOF, then proves the session still serves.
func finishSubscribe(ctx context.Context, client *moq.Client, broadcast string, frames int) error {
	announcements, err := client.Announced(moq.AnnounceOptions{Prefix: broadcast})
	if err != nil {
		return err
	}
	defer announcements.Cancel()
	announced, err := client.AnnouncedBroadcast(broadcast)
	if err != nil {
		return err
	}
	defer announced.Cancel()
	consumer, err := announced.Available(ctx)
	if err != nil {
		return err
	}
	catalogs, err := moqmedia.NewCatalogConsumer(ctx, consumer)
	if err != nil {
		return err
	}
	defer catalogs.Cancel()
	var catalog *moqmedia.Catalog
	for {
		catalog, err = catalogs.Next(ctx)
		if err != nil {
			return err
		}
		if catalog == nil {
			return errors.New("catalog ended without video")
		}
		if len(catalog.Video) != 0 {
			break
		}
	}
	var id string
	var video moqmedia.Video
	for id, video = range catalog.Video {
		break
	}
	media, err := moqmedia.NewTrackConsumer(ctx, consumer, moqmedia.TrackConfig{Rendition: moqmedia.VideoRendition(id, video), Subscription: &moq.Subscription{MaxDelay: durationPtr(maxDelay)}})
	if err != nil {
		return err
	}
	defer media.Cancel()
	count := 0
	for {
		read, err := media.Next(ctx)
		if err != nil {
			return err
		}
		if read == nil {
			break
		}
		if _, gap := read.(moqmedia.ReadGap); gap {
			return errors.New("media gap")
		}
		value, frameRead := read.(moqmedia.ReadFrame)
		if !frameRead {
			continue
		}
		frame := value.Frame
		// Empty payloads carry endpoint metadata, not codec frames.
		if len(frame.Payload) == 0 {
			continue
		}
		count++
		if count > frames {
			return fmt.Errorf("expected %d frames, received more", frames)
		}
	}
	if count != frames {
		return fmt.Errorf("expected %d frames, received %d", frames, count)
	}
	for {
		catalog, err := catalogs.Next(ctx)
		if err != nil {
			return err
		}
		if catalog == nil {
			break
		}
	}
	started := false
	for {
		event, err := announcements.Next(ctx)
		if err != nil {
			return err
		}
		if event == nil {
			return errors.New("announcement stream ended before broadcast END")
		}
		if start, ok := event.(moq.AnnounceEventStart); ok && start.Announce.Prefix == broadcast {
			started = true
		}
		if restart, ok := event.(moq.AnnounceEventRestart); ok && restart.Announce.Prefix == broadcast {
			return errors.New("broadcast restarted during finish")
		}
		if end, ok := event.(moq.AnnounceEventEnd); ok && end.Announce.Prefix == broadcast {
			if !started {
				return errors.New("broadcast ended without announcement")
			}
			break
		}
	}
	healthAnnounced, err := client.AnnouncedBroadcast(broadcast + ".health")
	if err != nil {
		return err
	}
	defer healthAnnounced.Cancel()
	health, err := healthAnnounced.Available(ctx)
	if err != nil {
		return err
	}
	live, err := health.SubscribeTrack(ctx, "live", nil)
	if err != nil {
		return err
	}
	defer live.Cancel()
	group, err := live.NextGroup(ctx)
	if err != nil {
		return err
	}
	if group == nil {
		return errors.New("missing session health group")
	}
	defer group.Cancel()
	frame, err := group.ReadFrame(ctx)
	if err != nil {
		return err
	}
	if frame == nil || string(frame.Payload) != "session-alive" || frame.Timestamp != nil {
		return errors.New("unexpected session health payload or timestamp")
	}
	frame, err = group.ReadFrame(ctx)
	if err != nil {
		return err
	}
	if frame != nil {
		return errors.New("session health group did not end after one frame")
	}
	next, err := live.NextGroup(ctx)
	if err != nil {
		return err
	}
	if next != nil {
		next.Cancel()
		return errors.New("session health track did not end after one group")
	}
	if client.Session().State().Connects != 1 {
		return errors.New("subscriber reconnected during finish")
	}
	fmt.Printf("finish clean frames=%d catalog=eof announce=end health=ok\n", frames)
	return nil
}

// decodeAudio decodes the first audioMs of the rendition to out.<codec>.{txt,pcm}, the layout
// tone.py reads. The codec is the catalog's up to its first `.` (`opus`, `mp4a`). The files are
// created exclusively, so two renditions of one codec fail rather than overwrite each other.
func decodeAudio(ctx context.Context, consumer *moq.BroadcastConsumer, id string, rendition moqmedia.Audio, out string, audioMs int) error {
	codec, _, _ := strings.Cut(rendition.Codec, ".")
	out += "." + codec
	decoder, err := moqaudio.Subscribe(ctx, consumer, id, rendition, moqaudio.DecoderOutput{
		Format:   moq.AudioSampleFormatF32,
		MaxDelay: durationPtr(maxDelay),
	})
	if err != nil {
		return err
	}
	defer decoder.Cancel()

	log, err := os.OpenFile(out+".txt", os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o644)
	if err != nil {
		return err
	}
	defer log.Close()
	pcm, err := os.OpenFile(out+".pcm", os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o644)
	if err != nil {
		return err
	}
	defer pcm.Close()

	fmt.Fprintf(log, "%d %d\n", rendition.SampleRate, rendition.ChannelCount)
	frameBytes := 4 * int(rendition.ChannelCount)
	decoded := 0
	for decoded*1000 < audioMs*int(rendition.SampleRate) {
		read, err := decoder.Next(ctx)
		if err != nil {
			return err
		}
		var frame moqaudio.Frame
		switch read := read.(type) {
		case moqaudio.ReadFrame:
			frame = read.Frame
		// moq-audio reports a hole it saw directly; tone.py checks the timestamps too.
		case moqaudio.ReadGap:
			return fmt.Errorf("audio gap after %d samples", decoded)
		default: // nil, the track's end, or a ReadEnd, the encoder's
			return errors.New("audio track ended early")
		}
		samples := len(frame.Data) / frameBytes

		fmt.Fprintf(log, "%d %d\n", frame.Timestamp.Microseconds(), samples)
		if _, err := pcm.Write(frame.Data); err != nil {
			return err
		}
		decoded += samples
	}
	return nil
}

func subscribe(ctx context.Context, url, broadcast string, opts dialOptions, timeout time.Duration, audioOut string, audioMs int, finish bool, frames int) error {
	parent := ctx
	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()
	client, err := dial(ctx, url, opts, timeout)
	if err != nil {
		return err
	}
	defer client.Close()
	if finish {
		fmt.Println("finish reader ready")
		command, err := bufio.NewReader(os.Stdin).ReadString('\n')
		if err != nil {
			return err
		}
		if command != "read\n" {
			return errors.New("expected finish read command")
		}
	}
	if finish {
		readCtx, cancel := context.WithTimeout(parent, timeout)
		defer cancel()
		return finishSubscribe(readCtx, client, broadcast, frames)
	}

	announced, err := client.AnnouncedBroadcast(broadcast)
	if err != nil {
		return err
	}
	defer announced.Cancel()

	consumer, err := announced.Available(ctx)
	if err != nil {
		return err
	}

	catalog, err := catalogWithMedia(ctx, consumer)
	if err != nil {
		return err
	}

	audioDone := make(chan error, len(catalog.Audio))
	for audioID, audio := range catalog.Audio {
		go func() { audioDone <- decodeAudio(ctx, consumer, audioID, audio, audioOut, audioMs) }()
	}

	var id string
	var video moqmedia.Video
	for id, video = range catalog.Video {
		break
	}

	media, err := moqmedia.NewTrackConsumer(ctx, consumer, moqmedia.TrackConfig{Rendition: moqmedia.VideoRendition(id, video), Subscription: &moq.Subscription{MaxDelay: durationPtr(maxDelay)}})
	if err != nil {
		return err
	}
	defer media.Cancel()

	total := 0
	for total == 0 {
		read, err := media.Next(ctx)
		if err != nil {
			return err
		}
		if read == nil {
			break
		}
		if _, gap := read.(moqmedia.ReadGap); gap {
			return errors.New("media gap")
		}
		value, frameRead := read.(moqmedia.ReadFrame)
		if !frameRead {
			continue
		}
		frame := value.Frame
		total += len(frame.Payload)
	}
	if total == 0 {
		return errors.New("no frame data received")
	}
	for range catalog.Audio {
		if err := <-audioDone; err != nil {
			return fmt.Errorf("audio: %w", err)
		}
	}
	fmt.Printf("received %d bytes from %q\n", total, broadcast)
	return nil
}

func run() error {
	if len(os.Args) < 2 {
		return errors.New("usage: go-interop publish|subscribe --url URL --broadcast PATH [--timeout SECONDS] [--version V] [--grant] [--tone HZ,...] [--audio-out PREFIX --audio-ms MS]")
	}
	role := os.Args[1]

	flags := flag.NewFlagSet(role, flag.ExitOnError)
	url := flags.String("url", "", "relay URL")
	broadcast := flags.String("broadcast", "", "broadcast path")
	timeout := flags.Float64("timeout", 20, "seconds to wait for data")
	version := flags.String("version", "", "offer only this version")
	grant := flags.Bool("grant", false, "wait for the grant the relay sends over AUTH and print it")
	finishAck := flags.String("finish-ack", "", "publisher completion FIFO; enables clean finish")
	finish := flags.Bool("finish", false, "verify a late tail and clean broadcast end")
	frames := flags.Int("frames", 10, "expected finish video frame count")
	tone := flags.String("tone", "", "publish: comma-separated frequency of each channel, in Hz")
	audioOut := flags.String("audio-out", "", "subscribe: decode each audio rendition to this prefix's .<codec>.txt and .pcm")
	audioMs := flags.Int("audio-ms", 0, "subscribe: how much audio to decode, in milliseconds")
	if err := flags.Parse(os.Args[2:]); err != nil {
		return err
	}
	if *url == "" || *broadcast == "" {
		return errors.New("--url and --broadcast are required")
	}

	if *frames <= 0 || (*finish && role != "subscribe") || (*finishAck != "" && role != "publish") {
		return errors.New("finish requires a subscriber, finish-ack a publisher, and frames must be positive")
	}

	// Warnings and errors from the Rust layer, such as a publication outside the grant.
	if err := moq.LogLevel("warn"); err != nil {
		return err
	}
	wait := time.Duration(*timeout * float64(time.Second))
	ctx := context.Background()
	opts := dialOptions{version: *version, grant: *grant}
	switch role {
	case "publish":
		var tones []float64
		for _, field := range strings.Split(*tone, ",") {
			hz, err := strconv.ParseFloat(field, 64)
			if err != nil {
				return fmt.Errorf("--tone: %w", err)
			}
			tones = append(tones, hz)
		}
		return publish(ctx, *url, *broadcast, opts, wait, tones, *finishAck)
	case "subscribe":
		if !*finish && (*audioOut == "" || *audioMs <= 0) {
			return errors.New("subscribe needs --audio-out and --audio-ms")
		}
		return subscribe(ctx, *url, *broadcast, opts, wait, *audioOut, *audioMs, *finish, *frames)
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
