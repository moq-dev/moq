package media_test

import (
	"errors"
	"testing"

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
	if err := broadcast.Close(); err != nil {
		t.Fatal(err)
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
