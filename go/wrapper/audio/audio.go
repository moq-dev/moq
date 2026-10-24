// Package audio owns native audio encoding, publication, and decoding.
package audio

import (
	"context"
	"fmt"
	"iter"
	"math"
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

// Preset is how the encoder trades latency for compression; a nil EncoderOutput.Preset is PresetBalanced.
type Preset = ffi.MoqAudioPreset

const (
	// PresetLowLatency packs 10 ms Opus packets, at twice the packet rate.
	PresetLowLatency = ffi.MoqAudioPresetLowLatency
	// PresetBalanced packs 20 ms Opus packets, the default.
	PresetBalanced = ffi.MoqAudioPresetBalanced
	// PresetQuality favors compression; 20 ms Opus packets today, the same as PresetBalanced.
	PresetQuality = ffi.MoqAudioPresetQuality
)

// EncoderOutput configures the codec side: codec, track name, rate, channels, bitrate, frame duration (0 takes the preset's or codec's own frame), and preset.
// Preset is refused alongside a nonzero FrameDuration.
type EncoderOutput struct {
	Codec         Codec
	Track         *string
	SampleRate    *uint32
	Channels      *uint32
	Bitrate       *uint32
	FrameDuration time.Duration
	Preset        *Preset
}

func (o EncoderOutput) ffi() (ffi.MoqAudioEncoderOutput, error) {
	frame, err := bridge.Duration(o.FrameDuration, "audio frame duration")
	if err == nil && frame > math.MaxUint32 {
		err = fmt.Errorf("audio frame duration %v exceeds %d microseconds", o.FrameDuration, uint32(math.MaxUint32))
	}
	return ffi.MoqAudioEncoderOutput{Codec: o.Codec, Track: o.Track, SampleRate: o.SampleRate, Channels: o.Channels, Bitrate: o.Bitrate, FrameDurationUs: uint32(frame), Preset: o.Preset}, err
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
}

func (o DecoderOutput) ffi() (ffi.MoqAudioDecoderOutput, error) {
	maxDelay, err := bridge.OptionalDuration(o.MaxDelay, "audio max delay")
	if err != nil {
		return ffi.MoqAudioDecoderOutput{}, err
	}
	delay, err := bridge.OptionalDuration(o.Delay, "audio delay")
	return ffi.MoqAudioDecoderOutput{Format: o.Format, SampleRate: o.SampleRate, Channels: o.Channels, MaxDelayUs: maxDelay, DelayUs: delay}, err
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

// Producer encodes PCM and publishes it as a track in a broadcast's catalog.
type Producer struct{ inner *ffi.MoqAudioProducer }

// NewProducer opens an audio track on the catalog's broadcast and registers its rendition; bandwidth may be nil.
func NewProducer(catalog *media.CatalogProducer, input EncoderInput, output EncoderOutput, bandwidth *moq.Bandwidth) (*Producer, error) {
	out, err := output.ffi()
	if err != nil {
		return nil, err
	}
	var bw **ffi.MoqBandwidth
	if bandwidth != nil {
		raw := bridge.Bandwidth(bandwidth)
		bw = &raw
	}
	inner, err := ffi.NewMoqAudioProducer(bridge.CatalogProducer(catalog), input, out, bw)
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

// NewEncoder opens a codec-only encoder; output.Track is ignored.
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

// Next returns the next decoded frame, or (nil, nil) when the track ends.
func (d *Consumer) Next(ctx context.Context) (*Frame, error) {
	raw, err := bridge.Call(ctx, d.inner.Cancel, d.inner.Next)
	if err != nil || raw == nil {
		return nil, err
	}
	value := frame(*raw)
	return &value, nil
}

// All ranges over decoded frames until the track ends or the loop breaks.
func (d *Consumer) All(ctx context.Context) iter.Seq2[*Frame, error] { return bridge.Seq(ctx, d.Next) }

// Delay is the playout delay to hold ahead of the playhead: the fixed one, or the estimate from every frame read so far.
// The estimate measures when Next returns, so call it again as soon as the previous frame is handled.
func (d *Consumer) Delay() time.Duration {
	return time.Duration(d.inner.DelayUs()) * time.Microsecond
}

// Cancel stops the stream.
func (d *Consumer) Cancel() { d.inner.Cancel() }
