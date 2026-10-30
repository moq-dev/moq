// Package audio owns native audio encoding, publication, and decoding.
package audio

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

// Codec selects the audio codec; build one with moq.OpusAudioCodec or moq.AacAudioCodec.
type Codec = *ffi.MoqAudioCodec

// SampleFormat is a raw PCM sample layout.
type SampleFormat = ffi.MoqAudioSampleFormat

// EncoderInput declares the PCM layout written to a Producer.
type EncoderInput = ffi.MoqAudioEncoderInput

// Packet is one encoded payload from an Encoder.
type Packet = ffi.MoqAudioPacket

// EncoderFinish holds the packets Encoder.Finish drains and the padded samples to discard.
type EncoderFinish = ffi.MoqAudioEncoderFinished

// EncoderOutput configures the codec side: codec, rate, channels, bitrate, and frame duration (0 takes the codec's own frame; 10 ms Opus halves the packetization delay).
type EncoderOutput struct {
	Codec         Codec
	SampleRate    *uint32
	Channels      *uint32
	Bitrate       *uint32
	FrameDuration time.Duration
}

func (o EncoderOutput) ffi() (ffi.MoqAudioEncoderOutput, error) {
	// Go's zero value is the native null: the codec's own frame.
	var frame *time.Duration
	if o.FrameDuration != 0 {
		frame = &o.FrameDuration
	}
	return ffi.MoqAudioEncoderOutput{Codec: o.Codec, SampleRate: o.SampleRate, Channels: o.Channels, Bitrate: o.Bitrate, FrameDuration: frame}, bridge.NonNegative(frame, "audio frame duration")
}

// DecoderOutput configures the PCM a Consumer delivers, how stale a group may get before it is skipped, and the playout delay.
type DecoderOutput struct {
	Format     SampleFormat
	SampleRate *uint32
	Channels   *uint32
	// MaxDelay bounds how stale a group may get before it is skipped; it adds no buffering.
	MaxDelay *time.Duration
	// Delay fixes the jitter buffer to hold ahead of the playhead; nil estimates it from arrival timing.
	Delay *time.Duration
	// Start is where to begin on a track that already holds groups; zero is media.DecodeStartOldest.
	// A live player sets media.DecodeStartLatest so a reopened decoder skips the cached backlog.
	Start media.DecodeStart
}

func (o DecoderOutput) ffi() (ffi.MoqAudioDecoderOutput, error) {
	if err := bridge.NonNegative(o.MaxDelay, "audio max delay"); err != nil {
		return ffi.MoqAudioDecoderOutput{}, err
	}
	// Go's zero value is the native null: the oldest group.
	var start *media.DecodeStart
	if o.Start != 0 {
		start = &o.Start
	}
	return ffi.MoqAudioDecoderOutput{Format: o.Format, SampleRate: o.SampleRate, Channels: o.Channels, MaxDelay: o.MaxDelay, Delay: o.Delay, Start: start}, bridge.NonNegative(o.Delay, "audio delay")
}

// Frame is interleaved PCM with its presentation timestamp.
type Frame struct {
	Timestamp time.Duration
	Data      []byte
}

func (f Frame) ffi() (ffi.MoqAudioFrame, error) {
	us, err := bridge.Duration(f.Timestamp, "audio timestamp")
	return ffi.MoqAudioFrame{TimestampUs: us, Data: f.Data}, err
}

func frame(raw ffi.MoqAudioFrame) Frame {
	return Frame{Timestamp: time.Duration(raw.TimestampUs) * time.Microsecond, Data: raw.Data}
}

// Read is one item a Consumer reads, in delivery order: a ReadFrame, ReadGap, or ReadEnd.
type Read interface{ read() }

// ReadFrame is decoded PCM.
type ReadFrame struct {
	Frame Frame
}

// ReadGap is media lost after what was already read: what follows does not continue it.
type ReadGap struct {
	// Timestamp is the furthest presentation point already read.
	Timestamp time.Duration
	// Duration is an upper bound on how long until the next frame resumes.
	Duration time.Duration
}

// ReadEnd is where the publisher stopped its encoder: it paused, switched source, or finished.
// Frames that follow and start before End are its remaining media; the first at or past End
// comes from a fresh encoder.
type ReadEnd struct {
	// End is where the stopped encoder's media stops.
	End time.Duration
}

func (ReadFrame) read() {}
func (ReadGap) read()   {}
func (ReadEnd) read()   {}

func audioRead(raw ffi.MoqAudioRead) (Read, error) {
	switch read := raw.(type) {
	case ffi.MoqAudioReadFrame:
		return ReadFrame{Frame: frame(read.Frame)}, nil
	case ffi.MoqAudioReadGap:
		return ReadGap{Timestamp: time.Duration(read.TimestampUs) * time.Microsecond, Duration: read.Duration}, nil
	case ffi.MoqAudioReadEnd:
		return ReadEnd{End: time.Duration(read.EndUs) * time.Microsecond}, nil
	}
	return nil, fmt.Errorf("unknown audio read: %T", raw)
}

// Producer encodes PCM and publishes it as a track in a broadcast's catalog.
type Producer struct{ inner *ffi.MoqAudioProducer }

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
}

// NewProducer opens an audio track on the catalog's broadcast and registers its rendition.
func NewProducer(catalog *media.CatalogProducer, options ProducerOptions) (*Producer, error) {
	out, err := options.Output.ffi()
	if err != nil {
		return nil, err
	}
	var bw **ffi.MoqAllocator
	if options.Bandwidth != nil {
		raw := bridge.Allocator(options.Bandwidth)
		bw = &raw
	}
	inner, err := ffi.NewMoqAudioProducer(bridge.CatalogProducer(catalog), ffi.MoqAudioProducerOptions{Input: options.Input, Output: out, Track: options.Track, Bandwidth: bw})
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

// Reanchor re-anchors the timeline to the next frame after an idle gap.
func (p *Producer) Reanchor() error { return p.inner.Reanchor() }

// Write encodes and publishes one PCM frame in the configured input layout.
func (p *Producer) Write(value Frame) error {
	raw, err := value.ffi()
	if err != nil {
		return err
	}
	return p.inner.Write(raw)
}

// Reservation returns this track's bandwidth reservation, or nil without one.
func (p *Producer) Reservation() *moq.Reservation {
	raw := p.inner.Reservation()
	if raw == nil || *raw == nil {
		return nil
	}
	return bridge.Reservation(*raw).(*moq.Reservation)
}

// Finish flushes pending samples and ends the track.
func (p *Producer) Finish() error { return p.inner.Finish() }

// Encoder encodes PCM without publishing it.
type Encoder struct{ inner *ffi.MoqAudioEncoder }

// NewEncoder opens a codec-only encoder.
func NewEncoder(output EncoderOutput) (*Encoder, error) {
	raw, err := output.ffi()
	if err != nil {
		return nil, err
	}
	inner, err := ffi.NewMoqAudioEncoder(raw)
	if err != nil {
		return nil, err
	}
	return &Encoder{inner}, nil
}

// FrameSize is the samples per channel each Encode call takes.
func (e *Encoder) FrameSize() (uint64, error) { return e.inner.FrameSize() }

// TrackInit is the OpusHead or AudioSpecificConfig that publishes these packets via media.NewAudioTrackProducer.
func (e *Encoder) TrackInit() (media.AudioInit, error) { return e.inner.TrackInit() }

// SetBitrate retunes the live encoder in bits per second.
func (e *Encoder) SetBitrate(value uint64) error { return e.inner.SetBitrate(value) }

// Encode encodes one codec-sized interleaved f32 frame.
func (e *Encoder) Encode(pcm []float32) (Packet, error) { return e.inner.Encode(pcm) }

// Finish pads an optional partial frame, drains the codec, and closes the encoder.
func (e *Encoder) Finish(pcm []float32) (EncoderFinish, error) { return e.inner.Finish(pcm) }

// Consumer subscribes to an audio rendition and yields decoded PCM.
type Consumer struct{ inner *ffi.MoqAudioConsumer }

// Subscribe subscribes to the catalog rendition id, following its broadcast and track.
func Subscribe(ctx context.Context, broadcast *moq.BroadcastConsumer, id string, rendition media.Audio, output DecoderOutput) (*Consumer, error) {
	if err := bridge.NonNegative(rendition.Jitter, "jitter"); err != nil {
		return nil, err
	}
	out, err := output.ffi()
	if err != nil {
		return nil, err
	}
	inner, err := ffi.MoqAudioConsumerSubscribe(ctx, bridge.BroadcastConsumer(broadcast), id, rendition, out)
	if err != nil {
		return nil, err
	}
	return &Consumer{inner}, nil
}

// Next returns the next decoded frame, gap, or encoder end, or (nil, nil) when the track ends.
func (d *Consumer) Next(ctx context.Context) (Read, error) {
	raw, err := bridge.Call(ctx, d.inner.Cancel, d.inner.Next)
	if err != nil || raw == nil {
		return nil, err
	}
	return audioRead(*raw)
}

// All ranges over every read until the track ends or the loop breaks.
func (d *Consumer) All(ctx context.Context) iter.Seq2[Read, error] { return bridge.Seq(ctx, d.Next) }

// Delay is the playout delay to hold ahead of the playhead: the fixed one, or the estimate from every frame read so far.
// The estimate measures when Next returns, so call it again as soon as the previous frame is handled.
func (d *Consumer) Delay() time.Duration {
	return d.inner.Delay()
}

// Cancel stops the stream.
func (d *Consumer) Cancel() { d.inner.Cancel() }
