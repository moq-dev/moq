package moq

import (
	"time"

	ffi "moq.dev/moq-ffi/moq"
	"moq.dev/moq/internal/bridge"
)

// moq-ffi spells every duration as integer microseconds, because not every
// target language has a duration type. These records mirror the generated ones
// with time.Duration fields and convert at the boundary.

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
// Timestamp is nil on an untimed track, on both publish and receive.
type Frame struct {
	Payload   []byte
	Timestamp *time.Duration
}

func (f Frame) ffi() (ffi.MoqFrame, error) {
	us, err := bridge.OptionalDuration(f.Timestamp, "timestamp")
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
// a non-latest group may fall behind before it is skipped (zero skips at once, and it
// caps every range), the ranges of the track to deliver (nil for everything MaxDelay
// still allows, while an empty non-nil slice asks for nothing), and the order the
// publisher sends pending groups in (nil for newest first).
type Subscription struct {
	Priority int8
	MaxDelay time.Duration
	Ranges   []Range
	Order    *Order
}

func (s Subscription) ffi() (ffi.MoqSubscription, error) {
	maxDelay, err := bridge.Duration(s.MaxDelay, "max delay")
	var ranges *[]ffi.MoqRange
	if s.Ranges != nil {
		ranges = &s.Ranges
	}
	return ffi.MoqSubscription{
		Priority:   s.Priority,
		MaxDelayUs: maxDelay,
		Ranges:     ranges,
		Order:      s.Order,
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
// non-latest group is cached (nil for no limit), how many times its first frame
// a group's later bytes may grow before a joining subscriber asks for a new group
// (nil never asks), and the timescale in ticks per second (nil means untimed on
// both publish and receive). Priority is higher first; the zero Priority is the
// midpoint a nil TrackInfo uses. Compress DEFLATEs each group on the wire to peers
// that support it; readers always see the original frames.
type TrackInfo struct {
	Priority  int8
	MaxAge    *time.Duration
	Refresh   *uint32
	Timescale *uint64
	Compress  bool
}

func trackInfoFFI(info *TrackInfo) (*ffi.MoqTrackInfo, error) {
	if info == nil {
		return nil, nil
	}
	maxAge, err := bridge.OptionalDuration(info.MaxAge, "max age")
	return &ffi.MoqTrackInfo{
		Priority:  info.Priority,
		MaxAgeUs:  maxAge,
		Refresh:   info.Refresh,
		Timescale: info.Timescale,
		Compress:  info.Compress,
	}, err
}

func trackInfoFromFFI(info ffi.MoqTrackInfo) TrackInfo {
	return TrackInfo{
		Priority:  info.Priority,
		MaxAge:    optFromMicros(info.MaxAgeUs),
		Refresh:   info.Refresh,
		Timescale: info.Timescale,
		Compress:  info.Compress,
	}
}

// ConnectionStats holds transport metrics for a session (RTT, bandwidth, byte
// and packet counters); each field is nil when unreported.
type ConnectionStats struct {
	RTT                  *time.Duration
	EstimatedSendRateBps *uint64
	BytesSent            *uint64
	BytesReceived        *uint64
	BytesLost            *uint64
	PacketsSent          *uint64
	PacketsReceived      *uint64
	PacketsLost          *uint64
	// Version is the negotiated protocol version, spelled like WithVersions.
	Version *string
}

func connectionStatsFromFFI(s ffi.MoqConnectionStats) ConnectionStats {
	return ConnectionStats{
		RTT:                  optFromMicros(s.RttUs),
		EstimatedSendRateBps: s.EstimatedSendRateBps,
		BytesSent:            s.BytesSent,
		BytesReceived:        s.BytesReceived,
		BytesLost:            s.BytesLost,
		PacketsSent:          s.PacketsSent,
		PacketsReceived:      s.PacketsReceived,
		PacketsLost:          s.PacketsLost,
		Version:              s.Version,
	}
}

// Grant is what the peer lets this side do, in this side's own paths. An empty
// pattern list grants nothing in that direction; "**" grants everything.
type Grant struct {
	// Publish lists the broadcast patterns this side may publish to the peer.
	Publish []string
	// Subscribe lists the broadcast patterns this side may subscribe to from the peer.
	Subscribe []string
	// Expires is how long until the grant lapses, measured when it was read, or nil for never.
	Expires *time.Duration
}

func grantFromFFI(g *ffi.MoqGrant) *Grant {
	if g == nil {
		return nil
	}
	return &Grant{Publish: g.Publish, Subscribe: g.Subscribe, Expires: optFromMicros(g.ExpiresUs)}
}
