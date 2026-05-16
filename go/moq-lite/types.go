// Package moq is an ergonomic Go wrapper around the moq-ffi UniFFI bindings.
// It mirrors the Python wrapper at /py/moq-lite, exposing idiomatic Go types
// (range-over-func iterators, context.Context cancellation) on top of the
// generated Moq* objects.
package moq

import (
	moqffi "github.com/moq-dev/moq/go/moq-ffi/moq"
)

// Frame is a single media frame.
type Frame = moqffi.MoqFrame

// Catalog describes the tracks available on a broadcast.
type Catalog = moqffi.MoqCatalog

// Video describes a video rendition.
type Video = moqffi.MoqVideo

// Audio describes an audio rendition.
type Audio = moqffi.MoqAudio

// Dimensions is the width/height of a video.
type Dimensions = moqffi.MoqDimensions

// Container is the encoding container for media frames.
type Container = moqffi.Container

// ContainerLegacy is the legacy moq-ffi container.
type ContainerLegacy = moqffi.ContainerLegacy

// ContainerCmaf is the CMAF container.
type ContainerCmaf = moqffi.ContainerCmaf
