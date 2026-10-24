package moq_test

import (
	"context"
	"math"
	"runtime"
	"testing"

	"moq.dev/moq"
)

// Retaining the old handle models a replacement before collection.
// Update(0) must hand back its share even while both handles and subscribers live.
func TestReservationUpdateReleasesSubscribedTrack(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
	defer cancel()
	relay := startRelay(t, ctx, "127.0.0.1:0")
	defer relay.server.Close()
	client, err := moq.Dial(ctx, "https://"+relay.addr,
		moq.WithTLSVerify(false), moq.WithReconnect(false))
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()
	// Dial returns while connecting; the grants need the live session's estimate.
	waitConnects(t, ctx, client.Session(), 1)
	// Grant assertions use a live QUIC estimate, but need no traffic or probing.
	bandwidth := client.Session().Bandwidth()
	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	defer broadcast.Close()

	reserve := func(name string, priority int8, ceiling uint64) (*moq.TrackDemand, *moq.Reservation) {
		t.Helper()
		track, err := broadcast.PublishTrack(name, &moq.TrackInfo{Priority: priority})
		if err != nil {
			t.Fatal(err)
		}
		consumer, err := track.Consume(nil)
		if err != nil {
			t.Fatal(err)
		}
		t.Cleanup(consumer.Cancel)
		// TrackDemand is weak, so keep the producer for the whole test.
		t.Cleanup(func() { runtime.KeepAlive(track) })
		demand, err := track.Demand()
		if err != nil {
			t.Fatal(err)
		}
		return demand, bandwidth.Reserve(demand, ceiling)
	}
	grant := func(reservation *moq.Reservation, want uint64) {
		t.Helper()
		got := reservation.Grant()
		if got == nil {
			t.Fatalf("grant unavailable, want %d", want)
		}
		if *got != want {
			t.Fatalf("grant = %d, want %d", *got, want)
		}
	}

	demand, retired := reserve("encoder", 1, math.MaxUint64)
	_, sibling := reserve("sibling", 0, 1)
	replacement := bandwidth.Reserve(demand, 1)
	// Even a retained predecessor must relinquish the allocation explicitly.
	// Priority makes this independent of the live estimate's exact value.
	grant(sibling, 0)
	retired.Update(0)
	grant(retired, 0)
	grant(replacement, 1)
	grant(sibling, 1)

	// Zero is reversible and repeated release is harmless.
	retired.Update(math.MaxUint64)
	grant(sibling, 0)
	retired.Update(0)
	retired.Update(0)
	grant(sibling, 1)
	if demand.Wanted() == nil {
		t.Fatal("release ended the track subscription")
	}
	runtime.KeepAlive(retired)
}
