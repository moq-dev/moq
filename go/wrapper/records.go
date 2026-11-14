package moq

import (
	"fmt"
	"math"
	"math/bits"
	"time"

	ffi "moq.dev/moq-ffi/moq"
	"moq.dev/moq/internal/bridge"
)

// Timestamp is an exact media time: Value units at Scale units per second, with Value
// at most 2^61 - 1 and Scale non-zero and at most 2^62 - 1. A read carries the track's
// scale, so a tick such as 90 kHz survives a read and republish exactly.
//
// A scale-less track has no time, so there Scale is nil and Value is the frame's group key
// (a datagram's sequence); build one with Unscaled. The zero Timestamp is Unscaled(0).
// Scale is a pointer, so == compares its address: compare two with reflect.DeepEqual.
type Timestamp = ffi.MoqTimestamp

// Unscaled is the scale-less timestamp at group key key, or a datagram's sequence.
func Unscaled(key uint64) Timestamp {
	return Timestamp{Value: key}
}

// TimestampFromScale is value units at scale units per second; a write refuses one past
// the bounds above with ErrTimeOverflow.
func TimestampFromScale(value, scale uint64) Timestamp {
	return Timestamp{Value: value, Scale: &scale}
}

// TimestampFromDuration converts d exactly, at nanosecond scale, refusing a negative d.
func TimestampFromDuration(d time.Duration) (Timestamp, error) {
	if d < 0 {
		return Timestamp{}, fmt.Errorf("negative timestamp: %v", d)
	}
	return TimestampFromScale(uint64(d), uint64(time.Second)), nil
}

// TimestampToDuration converts t to a time.Duration, rounded down to a whole
// nanosecond, saturating past time.Duration's ~292 years. It fails on an unscaled
// timestamp, which has no time, and on a zero Scale, which no read returns.
func TimestampToDuration(t Timestamp) (time.Duration, error) {
	if t.Scale == nil {
		return 0, fmt.Errorf("unscaled timestamp #%d has no time", t.Value)
	}
	scale := *t.Scale
	if scale == 0 {
		return 0, fmt.Errorf("timestamp with a zero scale")
	}
	hi, lo := bits.Mul64(t.Value, uint64(time.Second))
	if hi >= scale {
		return time.Duration(math.MaxInt64), nil
	}
	ns, _ := bits.Div64(hi, lo, scale)
	if ns > math.MaxInt64 {
		return time.Duration(math.MaxInt64), nil
	}
	return time.Duration(ns), nil
}

// Frame is a raw track frame: a payload and its presentation timestamp, on both publish
// and receive. On a scale-less track the timestamp is Unscaled at the frame's group key.
type Frame = ffi.MoqFrame

// Datagram is a best-effort track datagram as received: sequence number, timestamp
// (Unscaled at the sequence on a scale-less track), and payload.
type Datagram = ffi.MoqDatagram

// DatagramInfo fixes a datagram track's publisher priority and optional timestamp scale.
type DatagramInfo = ffi.MoqDatagramInfo

// Delay is how far behind the live edge a subscriber or decoder plays: where it starts, and
// how late it may fall. The zero Delay is moq-net's default, a stated zero for both, which
// starts at the newest group and skips a superseded group at once.
type Delay struct {
	// Min is the target latency: the playout or jitter-buffer delay held behind the live edge,
	// and where a cold join starts. A Min past Max starts at Max, since the publisher serves
	// nothing older. moq-lite-07 sends it upstream.
	Min time.Duration
	// MinUnstated leaves Min unstated, ignoring it. A decoder plays it as zero, while the
	// subscription keeps it unstated, so it never pulls the aggregate across subscribers to zero.
	MinUnstated bool
	// Max is how far a non-latest group may fall behind the live edge before it is skipped. Zero
	// skips immediately; a larger value tolerates that much reordering. It caps every range,
	// and bounds lateness only, never the start.
	Max time.Duration
	// MaxUnlimited ignores Max and sets no subscriber limit, leaving the publisher's max age
	// alone to bound it.
	MaxUnlimited bool
}

func (d Delay) ffi() (ffi.MoqDelay, error) {
	var out ffi.MoqDelay
	if !d.MinUnstated {
		out.Min = &d.Min
	}
	if !d.MaxUnlimited {
		out.Max = &d.Max
	}
	if err := bridge.NonNegative(out.Min, "min delay"); err != nil {
		return out, err
	}
	return out, bridge.NonNegative(out.Max, "max delay")
}

func delayFromFFI(d ffi.MoqDelay) Delay {
	out := Delay{MinUnstated: d.Min == nil, MaxUnlimited: d.Max == nil}
	if d.Min != nil {
		out.Min = *d.Min
	}
	if d.Max != nil {
		out.Max = *d.Max
	}
	return out
}

// Subscription holds subscriber-side delivery preferences: priority, the Delay (where a cold
// join starts and how late a non-latest group may fall before it is skipped), the ranges of
// the track to deliver (nil for everything Delay.Max still allows, while an empty non-nil
// slice asks for nothing), and the order the publisher sends pending groups in (nil for
// newest first).
//
// The zero Subscription is moq-net's defaults, the same as DefaultSubscription.
type Subscription struct {
	Priority int8
	Delay    Delay
	Ranges   []Range
	Order    *Order
}

// DefaultSubscription returns moq-net's defaults, whose zero Delay starts at the newest group
// and skips a superseded group at once. A nil *Subscription means the same.
func DefaultSubscription() Subscription {
	d := ffi.MoqSubscriptionDefaults()
	var ranges []Range
	if d.Ranges != nil {
		ranges = *d.Ranges
	}
	return Subscription{Priority: d.Priority, Delay: delayFromFFI(d.Delay), Ranges: ranges, Order: d.Order}
}

func (s Subscription) ffi() (ffi.MoqSubscription, error) {
	var ranges *[]ffi.MoqRange
	if s.Ranges != nil {
		ranges = &s.Ranges
	}
	delay, err := s.Delay.ffi()
	return ffi.MoqSubscription{
		Priority: s.Priority,
		Delay:    delay,
		Ranges:   ranges,
		Order:    s.Order,
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
// superseded group stays fresh, how many times its first frame a group's later
// bytes may grow before a joining subscriber asks for a new group (nil never
// asks), and the timescale in ticks per second (nil means untimed on both publish
// and receive). Priority is higher first; the zero Priority is the midpoint a nil
// TrackInfo uses. Compression compresses each group on the wire to peers that
// support it, nil sending frames as written; readers always see the original frames.
type TrackInfo struct {
	Priority int8
	// MaxAge is how long a superseded group stays fresh. Zero, the default, serves
	// only the live edge.
	MaxAge time.Duration
	// MaxAgeUnlimited ignores MaxAge and sets no publisher limit, leaving only the
	// cache to bound how long a superseded group is served.
	MaxAgeUnlimited bool
	Refresh         *uint32
	Timescale       *uint64
	Compression     *Compression
}

func trackInfoFFI(info *TrackInfo) (*ffi.MoqTrackInfo, error) {
	if info == nil {
		return nil, nil
	}
	out := &ffi.MoqTrackInfo{
		Priority:        info.Priority,
		MaxAgeUnlimited: info.MaxAgeUnlimited,
		Refresh:         info.Refresh,
		Timescale:       info.Timescale,
		Compression:     info.Compression,
	}
	if !info.MaxAgeUnlimited {
		out.MaxAge = &info.MaxAge
	}
	return out, bridge.NonNegative(out.MaxAge, "max age")
}

func trackInfoFromFFI(info ffi.MoqTrackInfo) TrackInfo {
	out := TrackInfo{
		Priority:        info.Priority,
		MaxAgeUnlimited: info.MaxAgeUnlimited,
		Refresh:         info.Refresh,
		Timescale:       info.Timescale,
		Compression:     info.Compression,
	}
	if info.MaxAge != nil && !info.MaxAgeUnlimited {
		out.MaxAge = *info.MaxAge
	}
	return out
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
	// DatagramsUnrouted counts datagrams received for a subscription with no datagram
	// track; nil on a version without datagrams.
	DatagramsUnrouted *uint64
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
		DatagramsUnrouted:    s.DatagramsUnrouted,
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
