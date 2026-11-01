package moq

import (
	"math"
	"time"

	ffi "moq.dev/moq-ffi/moq"
	"moq.dev/moq/internal/bridge"
)

// moq-ffi spells media timestamps as integer microseconds. These records mirror
// the generated ones with time.Duration timestamps and convert at the boundary.

// fromMicros saturates past time.Duration's ~292 years rather than wrapping to
// a negative duration.
func fromMicros(us uint64) time.Duration {
	if us > uint64(math.MaxInt64/int64(time.Microsecond)) {
		return time.Duration(math.MaxInt64)
	}
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
// a non-latest group may fall behind before it is skipped (zero skips at once, nil
// sets no subscriber limit and leaves the publisher's max age alone to bound it, and
// it caps every range), the ranges of the track to deliver (nil for everything
// MaxDelay still allows, while an empty non-nil slice asks for nothing), and the order
// the publisher sends pending groups in (nil for newest first).
//
// Start from DefaultSubscription for moq-net's defaults; the zero Subscription sets no
// max delay.
type Subscription struct {
	Priority int8
	MaxDelay *time.Duration
	Ranges   []Range
	Order    *Order
}

// DefaultSubscription returns moq-net's defaults, whose zero MaxDelay skips a superseded
// group at once. A nil *Subscription means the same; change a field of this one instead.
func DefaultSubscription() Subscription {
	d := ffi.MoqSubscriptionDefaults()
	var ranges []Range
	if d.Ranges != nil {
		ranges = *d.Ranges
	}
	return Subscription{Priority: d.Priority, MaxDelay: d.MaxDelay, Ranges: ranges, Order: d.Order}
}

func (s Subscription) ffi() (ffi.MoqSubscription, error) {
	var ranges *[]ffi.MoqRange
	if s.Ranges != nil {
		ranges = &s.Ranges
	}
	return ffi.MoqSubscription{
		Priority: s.Priority,
		MaxDelay: s.MaxDelay,
		Ranges:   ranges,
		Order:    s.Order,
	}, bridge.NonNegative(s.MaxDelay, "max delay")
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
	return &ffi.MoqTrackInfo{
		Priority:  info.Priority,
		MaxAge:    info.MaxAge,
		Refresh:   info.Refresh,
		Timescale: info.Timescale,
		Compress:  info.Compress,
	}, bridge.NonNegative(info.MaxAge, "max age")
}

func trackInfoFromFFI(info ffi.MoqTrackInfo) TrackInfo {
	return TrackInfo{
		Priority:  info.Priority,
		MaxAge:    info.MaxAge,
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
	// Version is the negotiated protocol version, spelled like ClientConfig.Versions.
	Version *string
}

func connectionStatsFromFFI(s ffi.MoqConnectionStats) ConnectionStats {
	return ConnectionStats{
		RTT:                  s.Rtt,
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
	return &Grant{Publish: g.Publish, Subscribe: g.Subscribe, Expires: g.Expires}
}
