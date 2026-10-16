// Package video owns native video encoding, publication, and decoding.
package video

import (
	"context"
	"iter"
	"time"

	"moq.dev/moq"
	ffi "moq.dev/moq-ffi/moq"
	"moq.dev/moq/internal/bridge"
	"moq.dev/moq/media"
)

type Codec = ffi.MoqVideoCodec
type PixelFormat = ffi.MoqVideoPixelFormat
type EncoderKind = ffi.MoqVideoEncoderKind
type EncoderInput = ffi.MoqVideoEncoderInput
type EncoderOutput = ffi.MoqVideoEncoderOutput
type Gop = ffi.MoqVideoGop

func Keyframe(interval uint32) Gop { return ffi.MoqVideoGopKeyframe{Interval: interval} }

type Frame struct {
	Timestamp time.Duration
	Data      []byte
}

func (f Frame) ffi() (ffi.MoqVideoFrame, error) {
	us, err := bridge.Duration(f.Timestamp, "video timestamp")
	return ffi.MoqVideoFrame{TimestampUs: us, Data: f.Data}, err
}

type Packet struct {
	Timestamp time.Duration
	Data      []byte
	Keyframe  bool
}

func packet(raw ffi.MoqVideoPacket) Packet {
	return Packet{Timestamp: time.Duration(raw.TimestampUs) * time.Microsecond, Data: raw.Data, Keyframe: raw.Keyframe}
}

type DecoderOutput struct {
	Resize   *media.Dimensions
	MaxDelay *time.Duration
	Surface  bool
}

func (o DecoderOutput) ffi() (ffi.MoqVideoDecoderOutput, error) {
	delay, err := bridge.OptionalDuration(o.MaxDelay, "video max delay")
	return ffi.MoqVideoDecoderOutput{Resize: o.Resize, MaxDelayUs: delay, Surface: o.Surface}, err
}

type Producer struct{ inner *ffi.MoqVideoProducer }

func NewProducer(broadcast *moq.BroadcastProducer, catalog *media.CatalogProducer, input EncoderInput, output EncoderOutput, bandwidth *moq.Bandwidth) (*Producer, error) {
	var bw **ffi.MoqBandwidth
	if bandwidth != nil {
		raw := bridge.Bandwidth(bandwidth)
		bw = &raw
	}
	inner, err := ffi.NewMoqVideoProducer(bridge.BroadcastProducer(broadcast), bridge.CatalogProducer(catalog), input, output, bw)
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
func (p *Producer) Write(value Frame) error {
	raw, err := value.ffi()
	if err != nil {
		return err
	}
	return p.inner.Write(raw)
}
func (p *Producer) Cut() error                    { return p.inner.Cut() }
func (p *Producer) SetBitrate(value uint64) error { return p.inner.SetBitrate(value) }
func (p *Producer) Reservation() *moq.Reservation {
	raw := p.inner.Reservation()
	if raw == nil || *raw == nil {
		return nil
	}
	return bridge.Reservation(*raw).(*moq.Reservation)
}
func (p *Producer) Finish() error { return p.inner.Finish() }

type Encoder struct{ inner *ffi.MoqVideoEncoder }

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
func (e *Encoder) Encode(value Frame) ([]Packet, error) {
	raw, err := value.ffi()
	if err != nil {
		return nil, err
	}
	result, err := e.inner.Encode(raw)
	return packets(result), err
}
func (e *Encoder) Cut() error                    { return e.inner.Cut() }
func (e *Encoder) SetBitrate(value uint64) error { return e.inner.SetBitrate(value) }
func (e *Encoder) Flush() ([]Packet, error)      { raw, err := e.inner.Flush(); return packets(raw), err }
func (e *Encoder) Finish() ([]Packet, error)     { raw, err := e.inner.Finish(); return packets(raw), err }

type Decoder struct{ inner *ffi.MoqVideoDecoder }

func NewDecoder(ctx context.Context, broadcast *moq.BroadcastConsumer, name string, rendition media.Video, output DecoderOutput) (*Decoder, error) {
	out, err := output.ffi()
	if err != nil {
		return nil, err
	}
	inner, err := ffi.MoqVideoDecode(ctx, bridge.BroadcastConsumer(broadcast), name, rendition, out)
	if err != nil {
		return nil, err
	}
	return &Decoder{inner}, nil
}
func (d *Decoder) Next(ctx context.Context) (*ffi.MoqVideoDecodedFrame, error) {
	raw, err := bridge.Call(ctx, d.inner.Cancel, d.inner.Next)
	if err != nil || raw == nil {
		return nil, err
	}
	return *raw, nil
}
func (d *Decoder) All(ctx context.Context) iter.Seq2[*ffi.MoqVideoDecodedFrame, error] {
	return bridge.Seq(ctx, d.Next)
}
func (d *Decoder) Cancel() { d.inner.Cancel() }
