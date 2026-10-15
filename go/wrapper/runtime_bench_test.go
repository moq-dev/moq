package moq_test

import (
	"bytes"
	"context"
	"fmt"
	"testing"
	"time"

	"moq.dev/moq"
)

// BenchmarkPublisher measures delivered payload through real QUIC sessions and
// generated Go callbacks. Each round waits for every subscriber, so dropping
// groups or merely buffering writes cannot improve the reported throughput.
func BenchmarkPublisher(b *testing.B) {
	for _, publishers := range []int{1, 4} {
		for _, subscribers := range []int{1, 4} {
			b.Run(fmt.Sprintf("publishers=%d/subscribers=%d", publishers, subscribers), func(b *testing.B) {
				ctx, cancel := context.WithCancel(context.Background())
				server, err := moq.Listen(ctx, "127.0.0.1:0", moq.WithTLSGenerate("localhost"))
				if err != nil {
					cancel()
					b.Fatal(err)
				}
				served := make(chan error, 1)
				go func() { served <- server.Serve(ctx) }()
				b.Cleanup(func() { cancel(); _ = server.Close(); <-served })
				url := "https://" + server.LocalAddr()
				dial := func() *moq.Client {
					client, err := moq.Dial(ctx, url, moq.WithTLSVerify(false))
					if err != nil {
						b.Fatal(err)
					}
					b.Cleanup(func() { _ = client.Close() })
					return client
				}
				tracks := make([]*moq.TrackProducer, publishers)
				consumers := make([]*moq.TrackConsumer, 0, publishers*subscribers)
				for p := range tracks {
					client := dial()
					name := fmt.Sprintf("runtime/%d", p)
					broadcast, err := client.CreateBroadcast(name)
					if err != nil {
						b.Fatal(err)
					}
					b.Cleanup(func() { _ = broadcast.Close() })
					tracks[p], err = broadcast.PublishTrack("payload", timedInfo())
					if err != nil {
						b.Fatal(err)
					}
					if err := broadcast.Announce(moq.Route{}); err != nil {
						b.Fatal(err)
					}
					for s := 0; s < subscribers; s++ {
						subscriber := dial()
						announced, err := subscriber.AnnouncedBroadcast(name)
						if err != nil {
							b.Fatal(err)
						}
						b.Cleanup(announced.Cancel)
						remote, err := announced.Available(ctx)
						if err != nil {
							b.Fatal(err)
						}
						consumer, err := remote.SubscribeTrack(ctx, "payload", nil)
						if err != nil {
							b.Fatal(err)
						}
						b.Cleanup(consumer.Cancel)
						consumers = append(consumers, consumer)
					}
				}
				const frames = 64
				payload := bytes.Repeat([]byte{0x5a}, 1024)
				b.SetBytes(int64(publishers * subscribers * frames * len(payload)))
				b.ResetTimer()
				for n := 0; n < b.N; n++ {
					done := make(chan error, len(tracks)+len(consumers))
					for _, consumer := range consumers {
						go func(consumer *moq.TrackConsumer) {
							group, err := consumer.RecvGroup(ctx)
							if err != nil {
								done <- err
								return
							}
							if group == nil {
								done <- fmt.Errorf("track ended before batch")
								return
							}
							defer group.Cancel()
							for f := 0; f < frames; f++ {
								frame, err := group.ReadFrame(ctx)
								if err != nil {
									done <- err
									return
								}
								want := time.Duration(n*frames+f) * time.Millisecond
								if frame == nil || frame.Timestamp == nil || *frame.Timestamp != want || !bytes.Equal(frame.Payload, payload) {
									done <- fmt.Errorf("missing, reordered, or corrupt frame %d", f)
									return
								}
							}
							done <- nil
						}(consumer)
					}
					for _, track := range tracks {
						go func(track *moq.TrackProducer) {
							group, err := track.AppendGroup()
							if err != nil {
								done <- err
								return
							}
							for f := 0; f < frames; f++ {
								if err := group.WriteFrame(moq.Frame{Payload: payload, Timestamp: ts(time.Duration(n*frames+f) * time.Millisecond)}); err != nil {
									done <- err
									return
								}
							}
							done <- group.Finish()
						}(track)
					}
					var failure error
					for range len(tracks) + len(consumers) {
						if err := <-done; err != nil {
							if failure == nil {
								failure = err
							}
							cancel()
						}
					}
					if failure != nil {
						b.Fatal(failure)
					}
				}
			})
		}
	}
}
