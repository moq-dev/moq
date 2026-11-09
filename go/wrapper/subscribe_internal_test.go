package moq

import (
	"testing"

	ffi "moq.dev/moq-ffi/moq"
)

// futureSurface stands in for an ffi surface variant this wrapper predates.
type futureSurface struct{}

func (futureSurface) Destroy() {}

func TestVideoSurfaceUnknown(t *testing.T) {
	if got := videoSurface(nil); got != nil {
		t.Fatalf("videoSurface(nil) = %#v, want nil", got)
	}
	var pixels ffi.MoqVideoSurface = ffi.MoqVideoSurfacePixelBuffer{Pointer: 7}
	if got := videoSurface(&pixels); got != (VideoSurfacePixelBuffer{Pointer: 7}) {
		t.Fatalf("videoSurface(PixelBuffer) = %#v", got)
	}
	var future ffi.MoqVideoSurface = futureSurface{}
	if got := videoSurface(&future); got != (VideoSurfaceUnknown{}) {
		t.Fatalf("videoSurface(future) = %#v, want VideoSurfaceUnknown", got)
	}
}
