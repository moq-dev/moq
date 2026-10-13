package moq

import (
	"fmt"
	"time"

	ffi "moq.dev/moq-ffi/moq"
)

// moq-ffi spells every duration as integer microseconds, because not every
// target language has a duration type. These records mirror the generated ones
// with time.Duration fields and convert at the boundary.

// micros converts d to microseconds, refusing a negative duration rather than
// letting it wrap when cast to uint64.
func micros(name string, d time.Duration) (uint64, error) {
	if d < 0 {
		return 0, fmt.Errorf("negative %s: %v", name, d)
	}
	return uint64(d.Microseconds()), nil
}

func optMicros(name string, d *time.Duration) (*uint64, error) {
	if d == nil {
		return nil, nil
	}
	us, err := micros(name, *d)
	return &us, err
}

func fromMicros(us uint64) time.Duration {
	return time.Duration(us) * time.Microsecond
}

func optFromMicros(us *uint64) *time.Duration {
	if us == nil {
		return nil
	}
	d := fromMicros(*us)
	return &d
}

// Frame is a raw track frame: a payload and its presentation timestamp.
// Timestamp is nil on a frame read from an untimed track; a raw track published
// here is timed, so writing one needs it.
type Frame struct {
	Payload   []byte
	Timestamp *time.Duration
}

func (f Frame) ffi() (ffi.MoqFrame, error) {
	us, err := optMicros("timestamp", f.Timestamp)
	return ffi.MoqFrame{Payload: f.Payload, TimestampUs: us}, err
}

func frameFromFFI(f *ffi.MoqFrame) *Frame {
	if f == nil {
		return nil
	}
	return &Frame{Payload: f.Payload, Timestamp: optFromMicros(f.TimestampUs)}
}

// Datagram is a best-effort track datagram as received: sequence number, timestamp
// (nil from an untimed track), and payload.
type Datagram struct {
	Sequence  uint64
	Timestamp *time.Duration
	Payload   []byte
}

func datagramFromFFI(d *ffi.MoqDatagram) *Datagram {
	if d == nil {
		return nil
	}
	return &Datagram{Sequence: d.Sequence, Timestamp: optFromMicros(d.TimestampUs), Payload: d.Payload}
}

// Subscription holds subscriber-side delivery preferences: priority, the max delay
// a non-latest group may fall behind before it is skipped (zero skips at once), and
// an optional group range (GroupStart a floor, GroupEnd exclusive).
type Subscription struct {
	Priority   uint8
	MaxDelay   time.Duration
	GroupStart *uint64
	GroupEnd   *uint64
}

func (s Subscription) ffi() (ffi.MoqSubscription, error) {
	maxDelay, err := micros("max delay", s.MaxDelay)
	return ffi.MoqSubscription{
		Priority:   s.Priority,
		MaxDelayUs: maxDelay,
		GroupStart: s.GroupStart,
		GroupEnd:   s.GroupEnd,
	}, err
}

func subscriptionFFI(s *Subscription) (*ffi.MoqSubscription, error) {
	if s == nil {
		return nil, nil
	}
	out, err := s.ffi()
	return &out, err
}

// TrackInfo holds publisher-side track properties: priority, how long a
// non-latest group is cached (nil for no limit), and the timescale in ticks per
// second (nil uses microseconds when publishing, and means the source declared no
// timeline on a received track). A zero Priority is the least urgent, not the
// default; set 127 for the midpoint a nil TrackInfo uses.
type TrackInfo struct {
	Priority  uint8
	MaxAge    *time.Duration
	Timescale *uint64
}

func trackInfoFFI(info *TrackInfo) (*ffi.MoqTrackInfo, error) {
	if info == nil {
		return nil, nil
	}
	maxAge, err := optMicros("max age", info.MaxAge)
	return &ffi.MoqTrackInfo{Priority: info.Priority, MaxAgeUs: maxAge, Timescale: info.Timescale}, err
}

func trackInfoFromFFI(info ffi.MoqTrackInfo) TrackInfo {
	return TrackInfo{Priority: info.Priority, MaxAge: optFromMicros(info.MaxAgeUs), Timescale: info.Timescale}
}

// ConnectionStats holds transport metrics for a session (RTT, bandwidth, byte
// and packet counters); each field is nil when unreported.
type ConnectionStats struct {
	RTT                  *time.Duration
	EstimatedSendRateBps *uint64
	EstimatedRecvRateBps *uint64
	BytesSent            *uint64
	BytesReceived        *uint64
	BytesLost            *uint64
	PacketsSent          *uint64
	PacketsReceived      *uint64
	PacketsLost          *uint64
}

func connectionStatsFromFFI(s ffi.MoqConnectionStats) ConnectionStats {
	return ConnectionStats{
		RTT:                  optFromMicros(s.RttUs),
		EstimatedSendRateBps: s.EstimatedSendRateBps,
		EstimatedRecvRateBps: s.EstimatedRecvRateBps,
		BytesSent:            s.BytesSent,
		BytesReceived:        s.BytesReceived,
		BytesLost:            s.BytesLost,
		PacketsSent:          s.PacketsSent,
		PacketsReceived:      s.PacketsReceived,
		PacketsLost:          s.PacketsLost,
	}
}
