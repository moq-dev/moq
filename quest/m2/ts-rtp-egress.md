# [M] TS over RTP legs that an ST 2022-7 receiver can merge

## Goal

`moq export ts` can send its output as RTP (MPEG-2 TS over RTP, RFC 2250),
and two `--sync` legs of one broadcast send the same RTP packets, headers
included, for every datagram both can fill: the same sequence number,
timestamp, SSRC, and payload. A receiver applying
[ST 2022-7](https://pub.smpte.org/latest/st2022-7/st2022-7-2019.pdf) then
reconstructs the common content from whichever leg delivers each datagram,
through complementary loss on the two paths.

Non-goals: the receiver's merge; a MoQ-side merge of two subscriptions,
which a relay's same-epoch failover already does at the object level; and
identity across a replaced broadcast.

## Plan

Decided 2026-10-13 by the maintainer: RTP identity is a
function of TS slot identity (`moq export ts --sync`) and the epoch, and of
nothing a leg mints.

- Datagrams are cut on the slot grid, so their boundaries fall at the same
  packets on every leg. The sequence number and the 90 kHz timestamp come
  from the datagram's position on the broadcast's media timeline, modulo
  their field widths, so they survive the 33-bit source wrap and a late
  join. The SSRC derives from the broadcast path and the route's epoch, so
  a replacement is a new RTP stream and two broadcasts sharing one epoch (a
  multi-stage import clones its process epoch across paths) differ, barring
  a 32-bit hash collision. RFC 3550's collision resolution would split the
  legs, so it does not apply. An epochless route (a session whose version carries no
  epoch) gives two legs nothing shared to derive it from, so synchronized
  RTP refuses one; settle whether an explicit SSRC lifts that.
- A leg never sends a datagram it cannot fill exactly. A packetizer that
  completes a datagram with stuffing, or primes a cushion with null-only
  datagrams after a stall, sends them under sequence numbers its partner
  fills with programme, and a receiver keeps whichever copy arrives first.
  Silence lets the partner's copy through. The renderer hands the
  packetizer which slots it filled with substitute stuffing, since the
  bytes alone cannot tell them from the source's own null packets.
- Datagrams go out on the export's own release; no wall-clock scheduling.
- Settle the sink's shape (a destination on `export ts`, or its own sink),
  and whether the passthrough export can be packetized the same way from a
  constant-rate source.

Tests: two legs fed one broadcast with different join offsets, delivery
skew, and complementary group loss send byte-identical RTP packets for every
datagram both send, and a reference ST 2022-7 selection over the two
reproduces every datagram either sent. A datagram neither leg can fill is
absent from both, never stuffing in one, and a datagram holding a slot one
leg substituted is absent from that leg. Sequence numbers and timestamps
wrap identically, a replacement or another path changes the SSRC, and an
epochless route is refused. Claims of ST 2022-7 compatibility cite the standard and an
RTP-level test, not TS identity.

Public API: an RTP output for TS export, in the shape settled above.
