// Package audio owns native audio encoding, publication, and decoding.
package audio

import (
	"context"
	"iter"
	"time"

	"moq.dev/moq"
	ffi "moq.dev/moq-ffi/moq"
	"moq.dev/moq/internal/bridge"
	"moq.dev/moq/media"
)

type Codec = *ffi.MoqAudioCodec
type SampleFormat = ffi.MoqAudioSampleFormat
type EncoderInput = ffi.MoqAudioEncoderInput
type Packet = ffi.MoqAudioPacket
type EncoderFinish = ffi.MoqAudioEncoderFinish

type EncoderOutput struct {
	Codec         Codec
	Track         *string
	SampleRate    *uint32
	Channels      *uint32
	Bitrate       *uint32
	FrameDuration time.Duration
}

func (o EncoderOutput) ffi() (ffi.MoqAudioEncoderOutput, error) {
	frame, err := bridge.Duration(o.FrameDuration, "audio frame duration")
	return ffi.MoqAudioEncoderOutput{Codec: o.Codec, Track: o.Track, SampleRate: o.SampleRate, Channels: o.Channels, Bitrate: o.Bitrate, FrameDurationUs: uint32(frame)}, err
}

type DecoderOutput struct {
	Format     SampleFormat
	SampleRate *uint32
	Channels   *uint32
	MaxDelay   *time.Duration
}

func (o DecoderOutput) ffi() (ffi.MoqAudioDecoderOutput, error) {
	delay, err := bridge.OptionalDuration(o.MaxDelay, "audio max delay")
	return ffi.MoqAudioDecoderOutput{Format: o.Format, SampleRate: o.SampleRate, Channels: o.Channels, MaxDelayUs: delay}, err
}

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

type Producer struct{ inner *ffi.MoqAudioProducer }

func NewProducer(broadcast *moq.BroadcastProducer, catalog *media.CatalogProducer, input EncoderInput, output EncoderOutput, bandwidth *moq.Bandwidth) (*Producer, error) {
	out, err := output.ffi()
	if err != nil {
		return nil, err
	}
	var bw **ffi.MoqBandwidth
	if bandwidth != nil {
		raw := bridge.Bandwidth(bandwidth)
		bw = &raw
	}
	inner, err := ffi.NewMoqAudioProducer(bridge.BroadcastProducer(broadcast), bridge.CatalogProducer(catalog), input, out, bw)
	if err != nil {
		return nil, err
	}
	return &Producer{inner}, nil
}
func (p *Producer) Demand() (*moq.TrackDemand, error) {
	inner, err := p.inner.Demand()
	if err != nil {
		return nil, err
	}
	return bridge.TrackDemand(inner).(*moq.TrackDemand), nil
}
func (p *Producer) Reanchor() error { return p.inner.Reanchor() }
func (p *Producer) Write(value Frame) error {
	raw, err := value.ffi()
	if err != nil {
		return err
	}
	return p.inner.Write(raw)
}
func (p *Producer) Reservation() *moq.Reservation {
	raw := p.inner.Reservation()
	if raw == nil || *raw == nil {
		return nil
	}
	return bridge.Reservation(*raw).(*moq.Reservation)
}
func (p *Producer) Finish() error { return p.inner.Finish() }

type Encoder struct{ inner *ffi.MoqAudioEncoder }

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
func (e *Encoder) FrameSize() (uint64, error)                  { return e.inner.FrameSize() }
func (e *Encoder) Encode(pcm []float32) (Packet, error)        { return e.inner.Encode(pcm) }
func (e *Encoder) Finish(pcm []float32) (EncoderFinish, error) { return e.inner.Finish(pcm) }

type Decoder struct{ inner *ffi.MoqAudioDecoder }

func NewDecoder(ctx context.Context, broadcast *moq.BroadcastConsumer, name string, rendition media.Audio, output DecoderOutput) (*Decoder, error) {
	out, err := output.ffi()
	if err != nil {
		return nil, err
	}
	inner, err := ffi.MoqAudioDecode(ctx, bridge.BroadcastConsumer(broadcast), name, rendition, out)
	if err != nil {
		return nil, err
	}
	return &Decoder{inner}, nil
}
func (d *Decoder) Next(ctx context.Context) (*Frame, error) {
	raw, err := bridge.Call(ctx, d.inner.Cancel, d.inner.Next)
	if err != nil || raw == nil {
		return nil, err
	}
	value := frame(*raw)
	return &value, nil
}
func (d *Decoder) All(ctx context.Context) iter.Seq2[*Frame, error] { return bridge.Seq(ctx, d.Next) }
func (d *Decoder) Cancel()                                          { d.inner.Cancel() }
