package media_test

import (
	"context"
	"errors"
	"testing"
	"time"

	"moq.dev/moq"
	"moq.dev/moq/media"
)

func TestCatalogClosesWithBroadcast(t *testing.T) {
	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	catalog, err := media.NewCatalogProducer(broadcast)
	if err != nil {
		t.Fatal(err)
	}
	if err := catalog.SetSection("app", map[string]int{"value": 42}); err != nil {
		t.Fatal(err)
	}
	if _, err := catalog.Now(); err != nil {
		t.Fatal(err)
	}
	if err := broadcast.Close(); err != nil {
		t.Fatal(err)
	}
	if _, err := catalog.Now(); !errors.Is(err, moq.ErrClosed) {
		t.Fatalf("now after close: %v", err)
	}
	if err := catalog.RemoveSection("app"); !errors.Is(err, moq.ErrClosed) {
		t.Fatalf("remove after close: %v", err)
	}
}

func TestRequestedTargetRequiresRequest(t *testing.T) {
	for _, target := range []media.Target{nil, media.Requested{}, (*media.Named)(nil), (*media.Requested)(nil)} {
		if _, err := media.NewAudioTrackProducer(nil, target, media.AudioInit{}); err == nil {
			t.Fatal("invalid target succeeded")
		}
	}
}

// The generated converter panics on a negative duration, so it is refused first.
func TestFetchGroupRefusesNegativeMaxDelay(t *testing.T) {
	negative := -time.Millisecond
	config := media.GroupConfig{Options: &moq.FetchGroupOptions{MaxDelay: &negative}}
	if _, err := media.FetchGroup(context.Background(), nil, config); err == nil {
		t.Fatal("a negative fetch max delay was accepted, want an error")
	}
}

func TestContainerTimebaseBelongsToOneBroadcast(t *testing.T) {
	owner, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	defer owner.Close()
	other, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	defer other.Close()
	timebase, err := media.NewTimebase(owner)
	if err != nil {
		t.Fatal(err)
	}
	defer timebase.Close()
	if _, err := media.NewContainerProducer(other, media.ContainerInit{Format: media.ContainerFormatFmp4, Timebase: timebase}); !errors.Is(err, moq.ErrCodec) {
		t.Fatalf("foreign timebase: %v", err)
	}
	if err := owner.Close(); err != nil {
		t.Fatal(err)
	}
	if _, err := media.NewTimebase(owner); !errors.Is(err, moq.ErrClosed) {
		t.Fatalf("closed owner: %v", err)
	}
}
