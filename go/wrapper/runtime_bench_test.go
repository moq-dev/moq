//go:build unix

package moq_test

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"sort"
	"strings"
	"sync"
	"syscall"
	"testing"
	"time"

	"moq.dev/moq"
)

// benchCase is one throughput case of the binding benchmark (see bench/README.md):
// publishers tracks, each read by subscribers consumers.
type benchCase struct{ size, publishers, subscribers int }

// Frame sizes at one publisher and subscriber, then fan-out at 1 KiB.
var benchCases = []benchCase{
	{100, 1, 1}, {1024, 1, 1}, {64 * 1024, 1, 1}, {1024 * 1024, 1, 1},
	{1024, 1, 4}, {1024, 4, 1}, {1024, 4, 4},
}

// benchCacheBytes is each case's origin cache budget: far above one round's groups, so
// eviction only reclaims groups every subscriber has read, and memory stays flat.
const benchCacheBytes uint64 = 64 << 20

// frames per group, holding a group under moq-net's 32 MB group cache.
func (c benchCase) frames() int { return min(max((8<<20)/c.size, 1), 64) }

func (c benchCase) name() string {
	return fmt.Sprintf("size=%d/pub=%d/sub=%d", c.size, c.publishers, c.subscribers)
}

// benchResult is one entry of the shared JSON report.
type benchResult struct {
	Name  string  `json:"name"`
	Unit  string  `json:"unit"`
	Value float64 `json:"value"`
}

// benchResults holds each benchmark's latest run: testing.B reruns a benchmark with a
// growing b.N, and the last run is the one it reports.
var benchResults sync.Map

func benchRecord(name, unit string, value float64) {
	benchResults.Store(name, benchResult{name, unit, value})
}

func cpuTime() time.Duration {
	var usage syscall.Rusage
	if err := syscall.Getrusage(syscall.RUSAGE_SELF, &usage); err != nil {
		panic(err)
	}
	return time.Duration(usage.Utime.Nano() + usage.Stime.Nano())
}

// TestMain writes BenchmarkFFI's results to $MOQ_BENCH_DIR/go.json after the run.
func TestMain(m *testing.M) {
	code := m.Run()
	if dir := os.Getenv("MOQ_BENCH_DIR"); dir != "" && code == 0 {
		var results []benchResult
		benchResults.Range(func(_, value any) bool {
			results = append(results, value.(benchResult))
			return true
		})
		sort.Slice(results, func(i, j int) bool { return results[i].Name < results[j].Name })
		data, err := json.MarshalIndent(results, "", "  ")
		if err == nil {
			err = os.MkdirAll(dir, 0o755)
		}
		if err == nil {
			err = os.WriteFile(filepath.Join(dir, "go.json"), data, 0o644)
		}
		if err != nil {
			fmt.Fprintln(os.Stderr, err)
			code = 1
		}
	}
	os.Exit(code)
}

// BenchmarkFFI is the Go runner of the binding benchmark: the same in-process rounds as
// every wrapper, so dividing by the Rust baselines isolates what Go adds.
func BenchmarkFFI(b *testing.B) {
	for _, c := range benchCases {
		b.Run("throughput/"+c.name(), func(b *testing.B) { benchThroughput(b, c) })
	}
	b.Run("call/sequence", func(b *testing.B) {
		track := benchTrack(b, "calls")
		producer, err := track.AppendGroup()
		if err != nil {
			b.Fatal(err)
		}
		group, err := producer.Consume()
		if err != nil {
			b.Fatal(err)
		}
		b.Cleanup(group.Cancel)
		benchCall(b, func() { group.Sequence() })
	})
	b.Run("call/info", func(b *testing.B) {
		consumer, err := benchTrack(b, "calls").Consume(nil)
		if err != nil {
			b.Fatal(err)
		}
		b.Cleanup(consumer.Cancel)
		benchCall(b, func() { _ = consumer.Info() })
	})
}

func benchTrack(b *testing.B, name string) *moq.TrackProducer {
	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		b.Fatal(err)
	}
	b.Cleanup(func() { _ = broadcast.Close() })
	track, err := broadcast.PublishTrack(name, nil)
	if err != nil {
		b.Fatal(err)
	}
	return track
}

func benchCall(b *testing.B, call func()) {
	b.ResetTimer()
	for range b.N {
		call()
	}
	benchRecord(strings.TrimPrefix(b.Name(), "BenchmarkFFI/"), "ns", float64(b.Elapsed().Nanoseconds())/float64(b.N))
}

// benchThroughput runs one round per iteration: each publisher writes a group from this
// goroutine in turn while every consumer reads it on its own goroutine. A round waits for
// every consumer, so buffering writes cannot improve the reported throughput.
func benchThroughput(b *testing.B, c benchCase) {
	ctx, cancel := context.WithCancel(context.Background())
	b.Cleanup(cancel)
	frames := c.frames()
	cacheBytes := benchCacheBytes
	origin := moq.NewOriginProducerWithConfig(moq.OriginConfig{CacheCapacityBytes: &cacheBytes})
	// The origin owns the cache policy; keep it until the case ends.
	b.Cleanup(func() { runtime.KeepAlive(origin) })
	publication, err := origin.CreateBroadcast("bench")
	if err != nil {
		b.Fatal(err)
	}
	broadcast := publication.Broadcast()
	b.Cleanup(func() { _ = broadcast.Close() })
	tracks := make([]*moq.TrackProducer, c.publishers)
	consumers := make([]*moq.TrackConsumer, 0, c.publishers*c.subscribers)
	for p := range tracks {
		tracks[p], err = broadcast.PublishTrack(fmt.Sprint(p), timedInfo())
		if err != nil {
			b.Fatal(err)
		}
		for range c.subscribers {
			consumer, err := tracks[p].Consume(nil)
			if err != nil {
				b.Fatal(err)
			}
			b.Cleanup(consumer.Cancel)
			consumers = append(consumers, consumer)
		}
	}
	// A non-uniform payload, so a zero-filled or truncated copy fails the check.
	payload := make([]byte, c.size)
	for i := range payload {
		payload[i] = byte(i % 251)
	}
	delivered := int64(len(consumers) * frames * c.size)
	b.SetBytes(delivered)

	round := func(n int) error {
		done := make(chan error, len(consumers))
		for _, consumer := range consumers {
			go func() {
				group, err := consumer.RecvGroup(ctx)
				if err == nil && group == nil {
					err = fmt.Errorf("track ended")
				}
				if err != nil {
					done <- err
					return
				}
				defer group.Cancel()
				for f := range frames {
					frame, err := group.ReadFrame(ctx)
					if err != nil {
						done <- err
						return
					}
					want := time.Duration(n*frames+f) * time.Microsecond
					if frame == nil || !bytes.Equal(frame.Payload, payload) || frame.Timestamp == nil || *frame.Timestamp != want {
						done <- fmt.Errorf("missing, reordered, or corrupt frame %d of round %d", f, n)
						return
					}
				}
				done <- nil
			}()
		}
		for _, track := range tracks {
			group, err := track.AppendGroup()
			if err != nil {
				return err
			}
			for f := range frames {
				timestamp := time.Duration(n*frames+f) * time.Microsecond
				if err := group.WriteFrame(moq.Frame{Payload: payload, Timestamp: &timestamp}); err != nil {
					return err
				}
			}
			if err := group.Finish(); err != nil {
				return err
			}
		}
		var failure error
		for range consumers {
			if err := <-done; err != nil && failure == nil {
				failure = err
				cancel()
			}
		}
		return failure
	}

	b.ResetTimer()
	cpu := cpuTime()
	for n := range b.N {
		if err := round(n); err != nil {
			b.Fatal(err)
		}
	}
	mb := float64(int64(b.N)*delivered) / 1e6
	cpuPerMB := (cpuTime() - cpu).Seconds() * 1e3 / mb
	b.ReportMetric(cpuPerMB, "cpu-ms/MB")
	name := c.name()
	benchRecord("throughput/"+name, "MB/s", mb/b.Elapsed().Seconds())
	benchRecord("cpu/"+name, "ms/MB", cpuPerMB)
}
