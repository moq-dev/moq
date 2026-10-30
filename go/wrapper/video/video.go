// Package video owns native video encoding, publication, and decoding.
package video

import (
	"context"
	"fmt"
	"iter"
	"time"

	"moq.dev/moq"
	ffi "moq.dev/moq-ffi/moq"
	"moq.dev/moq/internal/bridge"
	"moq.dev/moq/media"
)

// Codec selects H.264 or H.265.
type Codec = ffi.MoqVideoCodec

// PixelFormat is a CPU pixel layout (I420 or RGBA).
type PixelFormat = ffi.MoqVideoPixelFormat

// EncoderKind selects the encoder backend; build one with moq.AutoEncoder and friends.
type EncoderKind = ffi.MoqVideoEncoderKind

// FrameRate is an exact video frame rate in frames per second.
type FrameRate = ffi.MoqVideoFrameRate

// EncoderInput declares the pixel layout, resolution, and framerate of written frames.
type EncoderInput = ffi.MoqVideoEncoderInput

// EncoderOutput configures the codec, bitrate, group mode, and backend preference.
type EncoderOutput = ffi.MoqVideoEncoderOutput

// Gop is how pictures are split into independently decodable groups; a nil EncoderOutput.Gop keyframes every ~2 seconds.
type Gop = ffi.MoqVideoGop

// Keyframe starts a group with a keyframe every interval frames.
func Keyframe(interval uint32) Gop { return ffi.MoqVideoGopKeyframe{Interval: interval} }

// Frame is one raw picture with its presentation timestamp.
type Frame struct {
	Timestamp time.Duration
	Data      []byte
}

func (f Frame) ffi() (ffi.MoqVideoFrame, error) {
	us, err := bridge.Duration(f.Timestamp, "video timestamp")
	return ffi.MoqVideoFrame{TimestampUs: us, Data: f.Data}, err
}

// Packet is one encoded access unit from an Encoder.
type Packet struct {
	Timestamp time.Duration
	Data      []byte
	Keyframe  bool
}

func packet(raw ffi.MoqVideoPacket) Packet {
	return Packet{Timestamp: time.Duration(raw.TimestampUs) * time.Microsecond, Data: raw.Data, Keyframe: raw.Keyframe}
}

// DecoderOutput configures an optional resize, a max delay, and whether frames keep the decoder's surface (macOS only).
type DecoderOutput struct {
	Resize   *media.Dimensions
	MaxDelay *time.Duration
	Surface  bool
}

func (o DecoderOutput) ffi() (ffi.MoqVideoDecoderOutput, error) {
	return ffi.MoqVideoDecoderOutput{Resize: o.Resize, MaxDelay: o.MaxDelay, Surface: o.Surface}, bridge.NonNegative(o.MaxDelay, "video max delay")
}

// Producer encodes raw pictures and publishes them as a track in a broadcast's catalog.
type Producer struct{ inner *ffi.MoqVideoProducer }

// ProducerOptions holds input, codec settings, and publication ownership.
type ProducerOptions struct {
	// Input is the raw media layout the caller writes.
	Input EncoderInput
	// Output is the codec configuration.
	Output EncoderOutput
	// Track names the published track; nil derives a name from the codec.
	Track *string
	// Bandwidth reserves the track against a session allocator when non-nil.
	Bandwidth *moq.Allocator
	// Lower is the next rendition down a simulcast ladder: when non-nil, this one
	// disables while the grant cannot fund it and resumes once it recovers. Needs
	// Bandwidth and a bitrate above the lower rendition's.
	Lower *Producer
}

// NewProducer opens a video track on the catalog's broadcast and registers its rendition.
func NewProducer(catalog *media.CatalogProducer, options ProducerOptions) (*Producer, error) {
	var bw **ffi.MoqAllocator
	if options.Bandwidth != nil {
		raw := bridge.Allocator(options.Bandwidth)
		bw = &raw
	}
	var lower **ffi.MoqVideoProducer
	if options.Lower != nil {
		lower = &options.Lower.inner
	}
	inner, err := ffi.NewMoqVideoProducer(bridge.CatalogProducer(catalog), ffi.MoqVideoProducerOptions{Input: options.Input, Output: options.Output, Track: options.Track, Bandwidth: bw, Lower: lower})
	if err != nil {
		return nil, err
	}
	return &Producer{inner}, nil
}

// Demand returns a watch-only handle to whether the track has subscribers.
func (p *Producer) Demand() (*moq.TrackDemand, error) {
	inner, err := p.inner.Demand()
	if err != nil {
		return nil, err
	}
	return bridge.TrackDemand(inner).(*moq.TrackDemand), nil
}

// Write encodes and publishes one raw picture in the configured input layout.
func (p *Producer) Write(value Frame) error {
	raw, err := value.ffi()
	if err != nil {
		return err
	}
	return p.inner.Write(raw)
}

// Cut starts a new group at the next written picture.
func (p *Producer) Cut() error { return p.inner.Cut() }

// SetBitrate retunes the live encoder in bits per second.
func (p *Producer) SetBitrate(value uint64) error { return p.inner.SetBitrate(value) }

// Reservation returns this track's bandwidth reservation, or nil without one.
func (p *Producer) Reservation() *moq.Reservation {
	raw := p.inner.Reservation()
	if raw == nil || *raw == nil {
		return nil
	}
	return bridge.Reservation(*raw).(*moq.Reservation)
}

// Finish drains the encoder and ends the track.
func (p *Producer) Finish() error { return p.inner.Finish() }

// Encoder encodes raw pictures without publishing them.
type Encoder struct{ inner *ffi.MoqVideoEncoder }

// NewEncoder opens a codec-only encoder.
func NewEncoder(input EncoderInput, output EncoderOutput) (*Encoder, error) {
	inner, err := ffi.NewMoqVideoEncoder(input, output)
	if err != nil {
		return nil, err
	}
	return &Encoder{inner}, nil
}

func packets(raw []ffi.MoqVideoPacket) []Packet {
	out := make([]Packet, len(raw))
	for i := range raw {
		out[i] = packet(raw[i])
	}
	return out
}

// Encode encodes one picture, returning any access units now available.
func (e *Encoder) Encode(value Frame) ([]Packet, error) {
	raw, err := value.ffi()
	if err != nil {
		return nil, err
	}
	result, err := e.inner.Encode(raw)
	return packets(result), err
}

// Cut forces the next encoded picture to start a new group.
func (e *Encoder) Cut() error { return e.inner.Cut() }

// SetBitrate retunes the live encoder in bits per second.
func (e *Encoder) SetBitrate(value uint64) error { return e.inner.SetBitrate(value) }

// Flush drains buffered access units and keeps the encoder open.
func (e *Encoder) Flush() ([]Packet, error) { raw, err := e.inner.Flush(); return packets(raw), err }

// Finish drains and closes the encoder.
func (e *Encoder) Finish() ([]Packet, error) { raw, err := e.inner.Finish(); return packets(raw), err }

// Consumer subscribes to a video rendition and yields decoded frames.
type Consumer struct{ inner *ffi.MoqVideoConsumer }

// Subscribe subscribes to the catalog rendition id, following its broadcast and track.
func Subscribe(ctx context.Context, broadcast *moq.BroadcastConsumer, id string, rendition media.Video, output DecoderOutput) (*Consumer, error) {
	out, err := output.ffi()
	if err != nil {
		return nil, err
	}
	inner, err := ffi.MoqVideoConsumerSubscribe(ctx, bridge.BroadcastConsumer(broadcast), id, rendition, out)
	if err != nil {
		return nil, err
	}
	return &Consumer{inner}, nil
}

// Read is one item a Consumer reads, in delivery order: a ReadFrame, ReadGap, or ReadEnd.
type Read interface{ read() }

// ReadFrame is a decoded picture. Close its frame when done with it, since held
// frames hold decoder buffers.
type ReadFrame struct {
	Frame *moq.VideoDecodedFrame
}

// ReadGap is media lost after what was already read: every picture from before the
// loss precedes it, and what follows does not continue it.
type ReadGap struct {
	// Timestamp is the furthest presentation point already read.
	Timestamp time.Duration
	// Duration is an upper bound on how long until the next picture resumes.
	Duration time.Duration
}

// ReadEnd is where the publisher stopped its encoder: it paused, switched source, or
// finished. The stopped encoder's last pictures precede it; what follows comes from a
// fresh encoder.
type ReadEnd struct {
	// End is where the stopped encoder's media stops.
	End time.Duration
}

func (ReadFrame) read() {}
func (ReadGap) read()   {}
func (ReadEnd) read()   {}

// releaseRead destroys the decoded frame a read nobody received holds.
func releaseRead(raw *ffi.MoqVideoRead) {
	if raw == nil {
		return
	}
	if read, ok := (*raw).(ffi.MoqVideoReadFrame); ok {
		read.Frame.Destroy()
	}
}

// Next returns the next decoded frame, gap, or encoder end, or (nil, nil) when the
// track ends. Close each frame when done with it.
func (d *Consumer) Next(ctx context.Context) (Read, error) {
	raw, err := bridge.CallRelease(ctx, d.inner.Cancel, d.inner.Next, releaseRead)
	if err != nil || raw == nil {
		return nil, err
	}
	switch read := (*raw).(type) {
	case ffi.MoqVideoReadFrame:
		return ReadFrame{Frame: bridge.VideoDecodedFrame(read.Frame).(*moq.VideoDecodedFrame)}, nil
	case ffi.MoqVideoReadGap:
		return ReadGap{Timestamp: time.Duration(read.TimestampUs) * time.Microsecond, Duration: read.Duration}, nil
	case ffi.MoqVideoReadEnd:
		return ReadEnd{End: time.Duration(read.EndUs) * time.Microsecond}, nil
	}
	return nil, fmt.Errorf("unknown video read: %T", *raw)
}

// All ranges over every read until the track ends or the loop breaks.
func (d *Consumer) All(ctx context.Context) iter.Seq2[Read, error] {
	return bridge.Seq(ctx, d.Next)
}

// Cancel stops the stream.
func (d *Consumer) Cancel() { d.inner.Cancel() }
