use std::collections::VecDeque;
use std::time::{Duration, Instant};

use str0m::format::{Codec, CodecExtra};
use str0m::media::{MediaKind, Mid, Pt};
use str0m::net::Receive;
use str0m::rtp::rtcp::Rtcp;
use str0m::rtp::{ExtensionValues, RawPacket, RtpWrite, Ssrc};
use str0m::{Event, Input, Output, Reason, Rtc, RtcConfig, RtcError};

mod common;
use common::{PendingPacket, connect_l_r_with_rtc, init_crypto_default, init_log, progress};

struct VideoTest {
    sender: common::TestRtc,
    receiver: common::TestRtc,
    now: Instant,
    pt: Pt,
    repair_pt: Option<Pt>,
    drop_original: Option<u16>,
    dropped: usize,
    defer_rtx: bool,
    deferred_rtx: Vec<PendingPacket>,
}

/// Exercises receive-side frame assembly with a packet-mode sender and configurable receiver.
/// Routes datagrams in memory and advances time explicitly to test exact deadlines.
/// Can drop original RTP packets or delay RTX while still exchanging feedback.
impl VideoTest {
    fn new(receiver_config: RtcConfig, rtx: bool) -> Result<Self, RtcError> {
        Self::with_codec(receiver_config, rtx, Codec::Vp8)
    }

    fn with_codec(receiver_config: RtcConfig, rtx: bool, codec: Codec) -> Result<Self, RtcError> {
        init_crypto_default();
        let now = Instant::now();
        let sender_config = Rtc::builder().set_rtp_mode(true).enable_raw_packets(true);
        let (mut sender, mut receiver) = connect_l_r_with_rtc(
            sender_config.build(now),
            receiver_config.enable_raw_packets(true).build(now),
        );
        for _ in 0..1000 {
            if sender.is_connected() && receiver.is_connected() {
                break;
            }
            progress(&mut sender, &mut receiver)?;
        }
        assert!(sender.is_connected() && receiver.is_connected());
        let now = sender.last.max(receiver.last);
        let params = sender
            .codec_config()
            .find(|p| p.spec().codec == codec)
            .unwrap();
        let pt = params.pt();
        let repair_pt = params.resend();
        let mut test = Self {
            sender,
            receiver,
            now,
            pt,
            repair_pt,
            drop_original: None,
            dropped: 0,
            defer_rtx: false,
            deferred_rtx: Vec::new(),
        };
        test.add_video("video".into(), 1337.into(), rtx);
        test.tick(now)?;
        test.sender.events.clear();
        test.receiver.events.clear();
        Ok(test)
    }

    fn add_video(&mut self, mid: Mid, ssrc: Ssrc, rtx: bool) {
        let repair = rtx.then(|| (*ssrc + 1).into());
        self.sender
            .direct_api()
            .declare_media(mid, MediaKind::Video);
        self.sender
            .direct_api()
            .declare_stream_tx(ssrc, repair, mid, None);
        self.sender
            .direct_api()
            .stream_tx(&ssrc)
            .unwrap()
            .set_rtx_cache(if rtx { 32 } else { 0 }, Duration::from_secs(3), None);
        self.receiver
            .direct_api()
            .declare_media(mid, MediaKind::Video);
        self.receiver
            .direct_api()
            .expect_stream_rx(ssrc, repair, mid, None)
            .suppress_nack(!rtx);
    }

    fn drain(
        rtc: &mut common::TestRtc,
        now: Instant,
        is_sender: bool,
        packets: &mut VecDeque<(bool, PendingPacket)>,
    ) -> Result<(), RtcError> {
        for _ in 0..1000 {
            match rtc.poll_output()? {
                Output::Timeout(at) => {
                    rtc.last = at;
                    return Ok(());
                }
                Output::Event(event) => rtc.events.push((now, event)),
                Output::Transmit(v) => packets.push_back((
                    is_sender,
                    PendingPacket {
                        proto: v.proto,
                        source: v.source,
                        destination: v.destination,
                        contents: v.contents.to_vec(),
                    },
                )),
            }
        }
        panic!("output polling did not quiesce");
    }

    // Route all traffic, including NACK/RTX, without the common harness's forced ticks.
    fn flush(&mut self) -> Result<(), RtcError> {
        let mut packets = VecDeque::new();
        Self::drain(&mut self.sender, self.now, true, &mut packets)?;
        Self::drain(&mut self.receiver, self.now, false, &mut packets)?;
        for _ in 0..10000 {
            let Some((from_sender, packet)) = packets.pop_front() else {
                return Ok(());
            };
            let bytes = &packet.contents;
            // The fixed RTP header is unencrypted; match only the original stream.
            if from_sender
                && bytes.len() >= 12
                && bytes[0] >> 6 == 2
                && bytes[1] & 0x7f == *self.pt
                && u32::from_be_bytes(bytes[8..12].try_into().unwrap()) == 1337
                && self.drop_original == Some(u16::from_be_bytes([bytes[2], bytes[3]]))
            {
                self.drop_original = None;
                self.dropped += 1;
                continue;
            }
            if from_sender
                && self.defer_rtx
                && bytes.len() >= 12
                && bytes[0] >> 6 == 2
                && Some(bytes[1] & 0x7f) == self.repair_pt.map(|pt| *pt)
                && u32::from_be_bytes(bytes[8..12].try_into().unwrap()) == 1338
            {
                self.deferred_rtx.push(packet);
                continue;
            }
            let to = if from_sender {
                &mut self.receiver
            } else {
                &mut self.sender
            };
            to.handle_input(Input::Receive(
                self.now,
                Receive {
                    proto: packet.proto,
                    source: packet.source,
                    destination: packet.destination,
                    contents: bytes.as_slice().try_into()?,
                },
            ))?;
            // Drain the destination before delivering another packet to that peer.
            Self::drain(to, self.now, !from_sender, &mut packets)?;
        }
        panic!("datagram exchange did not quiesce");
    }

    fn tick(&mut self, now: Instant) -> Result<(), RtcError> {
        assert!(now >= self.now);
        self.now = now;
        self.sender.handle_input(Input::Timeout(now))?;
        self.flush()?;
        self.receiver.handle_input(Input::Timeout(now))?;
        self.flush()
    }

    fn advance_to(&mut self, target: Instant) -> Result<(), RtcError> {
        assert!(target >= self.now);
        let mut same_time = 0;
        for _ in 0..10000 {
            let next = self.sender.last.min(self.receiver.last);
            if next > target {
                return self.tick(target);
            }
            assert!(next >= self.now);
            same_time = if next == self.now { same_time + 1 } else { 0 };
            assert!(
                same_time < 8,
                "expired timeout was rearmed: sender {:?}, receiver {:?}",
                self.sender.last_timeout_reason(),
                self.receiver.last_timeout_reason()
            );
            self.tick(next)?;
        }
        panic!("too many timer wakeups");
    }

    fn write(
        &mut self,
        ssrc: Ssrc,
        seq: u64,
        timestamp: u32,
        data: &[u8],
        marker: bool,
    ) -> Result<(), RtcError> {
        self.sender
            .direct_api()
            .stream_tx(&ssrc)
            .unwrap()
            .write_rtp(
                RtpWrite::new(self.pt, seq.into(), timestamp, self.now, data.to_vec())
                    .marker(marker)
                    .nackable(true),
            );
        self.flush()?;
        self.sender.handle_input(Input::Timeout(self.now))?;
        self.flush()
    }

    fn send_vp8_frame(&mut self, seq: u64) -> Result<(), RtcError> {
        self.write(
            1337.into(),
            seq,
            47_000_000 + (seq as u32 - 47_000) * 18_000,
            &[0x10, 0x00, 0x00, seq as u8],
            true,
        )
    }

    fn received_frames(&self) -> Vec<(u64, u64, bool)> {
        self.receiver
            .events
            .iter()
            .filter_map(|(_, event)| match event {
                Event::MediaData(data) => Some((
                    **data.seq_range.start(),
                    **data.seq_range.end(),
                    data.contiguous,
                )),
                _ => None,
            })
            .collect()
    }
}

#[test]
pub fn audio_start_of_talk_spurt() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let now = Instant::now();
    let rtc1 = Rtc::builder().set_rtp_mode(true).build(now);
    let rtc2 = Rtc::builder().set_reordering_size_audio(0).build(now);

    let (mut l, mut r) = connect_l_r_with_rtc(rtc1, rtc2);

    let mid = "audio".into();
    let ssrc_tx: Ssrc = 1337.into();

    l.direct_api().declare_media(mid, MediaKind::Audio);
    l.direct_api().declare_stream_tx(ssrc_tx, None, mid, None);
    r.direct_api().declare_media(mid, MediaKind::Audio);

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_opus();
    let ssrc = l.direct_api().stream_tx_by_mid(mid, None).unwrap().ssrc();
    assert_eq!(params.spec().codec, Codec::Opus);
    let pt = params.pt();

    let to_write: Vec<&[u8]> = vec![
        // 1
        &[0x1, 0x2, 0x3, 0x4],
        // 3
        &[0x9, 0xa, 0xb, 0xc],
        // 2
        &[0x5, 0x6, 0x7, 0x8],
    ];

    let mut to_write: VecDeque<_> = to_write.into();

    let mut write_at = l.last + Duration::from_millis(300);

    let mut counts: Vec<u64> = vec![0, 3, 1];

    loop {
        if l.start + l.duration() > write_at {
            write_at = l.last + Duration::from_millis(300);
            if let Some(packet) = to_write.pop_front() {
                let wallclock = l.start + l.duration();

                let mut direct = l.direct_api();
                let stream = direct.stream_tx(&ssrc).unwrap();

                let count = counts.remove(0);
                let time = (count * 1000 + 47_000_000) as u32;
                let seq_no = (47_000 + count).into();

                let exts = ExtensionValues {
                    audio_level: Some(-42 - count as i8),
                    voice_activity: Some(false),
                    ..Default::default()
                };

                stream.write_rtp(
                    RtpWrite::new(pt, seq_no, time, wallclock, packet)
                        .marker(*seq_no % 2 == 0)
                        .ext_vals(exts),
                );
            }
        }

        progress(&mut l, &mut r)?;

        if l.duration() > Duration::from_secs(10) {
            break;
        }
    }

    let media: Vec<_> = r
        .events
        .iter()
        .filter_map(|(_, e)| {
            if let Event::MediaData(v) = e {
                Some(v)
            } else {
                None
            }
        })
        .collect();

    for m in media {
        assert!(m.audio_start_of_talk_spurt == (**m.seq_range.start() % 2 == 0));
    }

    Ok(())
}

/// Test permanent packet loss releases video at the deadline and reports the gap.
#[test]
pub fn video_reorder_timeout_permanent_loss() -> Result<(), RtcError> {
    let timeout = Duration::from_millis(300);
    for cadence in [Duration::from_millis(200), Duration::from_secs(1)] {
        let mut t = VideoTest::new(Rtc::builder(), false)?;
        t.send_vp8_frame(47_000)?;
        assert_eq!(t.received_frames(), [(47_000, 47_000, true)]);
        t.advance_to(t.now + cadence)?;
        t.send_vp8_frame(47_002)?; // 47001 is never cached, so loss is permanent.
        let deadline = t.now + timeout;
        t.advance_to(deadline - Duration::from_nanos(1))?;
        assert_eq!(t.received_frames(), [(47_000, 47_000, true)]);
        assert_eq!(t.receiver.last, deadline);
        assert_eq!(t.receiver.last_timeout_reason(), Reason::ReceiveReorder);
        t.tick(t.receiver.last)?;
        assert_eq!(
            t.received_frames(),
            [(47_000, 47_000, true), (47_002, 47_002, false)]
        );
        t.send_vp8_frame(47_003)?;
        assert_eq!(
            t.received_frames(),
            [
                (47_000, 47_000, true),
                (47_002, 47_002, false),
                (47_003, 47_003, true)
            ]
        );
    }
    Ok(())
}

/// Test timeout expiry preserves VP8/VP9 temporal-layer dependency checks.
#[test]
fn video_reorder_timeout_temporal_dependencies() -> Result<(), RtcError> {
    let timeout = Duration::from_millis(300);
    for codec in [Codec::Vp8, Codec::Vp9] {
        let mut t = VideoTest::with_codec(Rtc::builder(), false, codec)?;
        // Parser-valid synthetic payloads, not complete encoded video bitstreams.
        let payload = |picture: u8, tl0: u8, layer: u8, keyframe: bool| match codec {
            // X/S, I/L/T, PictureID, TL0PICIDX, TID/Y, payload header.
            Codec::Vp8 => vec![
                0x90,
                0xe0,
                picture,
                tl0,
                layer << 6,
                u8::from(!keyframe),
                0,
                0,
            ],
            // I/P/L/B/E, PictureID, TID (SID=0), TL0PICIDX; non-flexible mode.
            Codec::Vp9 => vec![
                if keyframe { 0xac } else { 0xec },
                picture,
                layer << 5,
                tl0,
                if keyframe { 0x80 } else { 0x84 },
            ],
            _ => unreachable!(),
        };
        t.write(1337.into(), 47_000, 1000, &payload(1, 1, 0, true), true)?;
        t.write(1337.into(), 47_001, 2000, &payload(2, 1, 1, false), true)?;
        let initial = [(47_000, 47_000, true), (47_001, 47_001, true)];
        assert_eq!(t.received_frames(), initial);
        t.advance_to(t.now + Duration::from_millis(100))?;
        // Base picture 3 (sequence 47002, TL0PICIDX=2) is permanently missing.
        // Enhancement picture 4 depends on that missing base.
        t.write(1337.into(), 47_003, 4000, &payload(4, 2, 1, false), true)?;
        t.write(1337.into(), 47_004, 5000, &payload(5, 3, 0, true), true)?;
        let deadline = t.now + timeout;
        t.advance_to(deadline - Duration::from_nanos(1))?;
        assert_eq!(t.received_frames(), initial);
        assert_eq!(t.receiver.last, deadline);
        assert_eq!(t.receiver.last_timeout_reason(), Reason::ReceiveReorder);
        // A rejected candidate requires Session to compute the following candidate's deadline.
        t.advance_to(deadline)?;
        assert_eq!(
            t.received_frames(),
            [
                (47_000, 47_000, true),
                (47_001, 47_001, true),
                (47_004, 47_004, false)
            ],
            "{codec:?}: reject the enhancement and drain the following due base"
        );
        assert!(
            t.receiver.last > deadline,
            "no stale deadline after codec rejection"
        );
        t.write(1337.into(), 47_005, 6000, &payload(6, 3, 1, false), true)?;
        assert_eq!(t.received_frames().last(), Some(&(47_005, 47_005, true)));
        let layers: Vec<_> = t
            .receiver
            .events
            .iter()
            .filter_map(|(_, event)| match event {
                Event::MediaData(data) => Some(match data.codec_extra {
                    CodecExtra::Vp8(e) => (
                        e.picture_id.unwrap(),
                        e.tl0_picture_id.unwrap(),
                        e.layer_index,
                        e.is_keyframe,
                    ),
                    CodecExtra::Vp9(e) => (
                        e.picture_id.unwrap(),
                        e.tl0_picture_id.unwrap(),
                        e.tid.unwrap(),
                        e.is_keyframe,
                    ),
                    _ => panic!("expected parsed temporal-layer metadata"),
                }),
                _ => None,
            })
            .collect();
        assert_eq!(
            layers,
            [
                (1, 1, 0, true),
                (2, 1, 1, false),
                (5, 3, 0, true),
                (6, 3, 1, false)
            ]
        );
        t.advance_to(deadline + Duration::from_millis(100))?;
        assert_eq!(t.received_frames().len(), 4);
        assert_ne!(t.receiver.last_timeout_reason(), Reason::ReceiveReorder);
    }
    Ok(())
}

/// Test H264 FU-A reassembly waits for completion and uses the first fragment's receipt time.
#[test]
fn video_reorder_timeout_h264_fragments() -> Result<(), RtcError> {
    let timeout = Duration::from_millis(300);
    let mut t = VideoTest::with_codec(Rtc::builder(), false, Codec::H264)?;
    // Synthetic NAL/FU-A payloads exercise reassembly, not video decoding.
    t.write(1337.into(), 47_000, 1000, &[0x65, 0xaa], true)?;
    t.advance_to(t.now + Duration::from_millis(100))?;
    t.write(1337.into(), 47_002, 2000, &[0x7c, 0x85, 0x11], false)?;
    let deadline = t.now + timeout;
    t.write(1337.into(), 47_003, 2000, &[0x7c, 0x05, 0x22], false)?;
    t.advance_to(t.now + Duration::from_millis(100))?;
    assert_eq!(t.received_frames(), [(47_000, 47_000, true)]);
    assert_ne!(t.receiver.last_timeout_reason(), Reason::ReceiveReorder);
    t.write(1337.into(), 47_004, 2000, &[0x7c, 0x45, 0x33], true)?;
    t.advance_to(deadline - Duration::from_nanos(1))?;
    assert_eq!(t.received_frames(), [(47_000, 47_000, true)]);
    assert_eq!(t.receiver.last, deadline);
    assert_eq!(t.receiver.last_timeout_reason(), Reason::ReceiveReorder);
    t.tick(deadline)?;
    assert_eq!(
        t.received_frames(),
        [(47_000, 47_000, true), (47_002, 47_004, false)]
    );
    let data = t
        .receiver
        .events
        .iter()
        .find_map(|(_, event)| match event {
            Event::MediaData(data) if **data.seq_range.start() == 47_002 => Some(data),
            _ => None,
        })
        .unwrap();
    assert_eq!(data.data.as_ref(), &[0, 0, 0, 1, 0x65, 0x11, 0x22, 0x33]);
    assert_eq!(data.network_time, deadline - timeout);
    assert!(matches!(data.codec_extra, CodecExtra::H264(e) if e.is_keyframe));
    t.write(1337.into(), 47_005, 3000, &[0x61, 0x44], true)?;
    t.advance_to(t.now + Duration::from_millis(100))?;
    assert_eq!(t.received_frames().last(), Some(&(47_005, 47_005, true)));
    assert_eq!(t.received_frames().len(), 3);
    assert_ne!(t.receiver.last_timeout_reason(), Reason::ReceiveReorder);
    Ok(())
}

/// A slow H264 frame survives until it completes when no complete successor exists.
#[test]
fn video_reorder_timeout_h264_slow_frame_completes() -> Result<(), RtcError> {
    let mut t = VideoTest::with_codec(Rtc::builder(), false, Codec::H264)?;
    t.write(1337.into(), 47_000, 1000, &[0x65, 0xaa], true)?;
    t.write(1337.into(), 47_002, 2000, &[0x7c, 0x85, 0x11], false)?;
    t.advance_to(t.now + Duration::from_secs(5))?;
    t.write(1337.into(), 47_003, 2000, &[0x7c, 0x45, 0x33], true)?;
    assert_eq!(
        t.received_frames(),
        [(47_000, 47_000, true), (47_002, 47_003, false)]
    );
    t.write(1337.into(), 47_004, 3000, &[0x61, 0x44], true)?;
    t.advance_to(t.now + Duration::from_millis(600))?;
    assert_eq!(
        t.received_frames(),
        [
            (47_000, 47_000, true),
            (47_002, 47_003, false),
            (47_004, 47_004, true)
        ]
    );
    Ok(())
}

/// Test H264 can release a later NAL after earlier fragments of the same picture are lost.
#[test]
fn video_reorder_timeout_h264_same_timestamp_gap() -> Result<(), RtcError> {
    let timeout = Duration::from_millis(300);
    let mut t = VideoTest::with_codec(Rtc::builder(), false, Codec::H264)?;
    t.write(1337.into(), 47_000, 1000, &[0x65, 0xaa], true)?;
    t.write(1337.into(), 47_001, 2000, &[0x7c, 0x81, 0x11], false)?;
    // Missing FU-A fragment 47002; another NAL in the same picture is a new head.
    t.write(1337.into(), 47_003, 2000, &[0x7c, 0x41, 0x33], false)?;
    t.write(1337.into(), 47_004, 2000, &[0x61, 0x44], true)?;
    let deadline = t.now + timeout;
    t.advance_to(deadline - Duration::from_nanos(1))?;
    assert_eq!(t.received_frames(), [(47_000, 47_000, true)]);
    assert_eq!(t.receiver.last, deadline);
    t.tick(deadline)?;
    // The existing assembler can emit the later NAL despite missing earlier fragments.
    assert_eq!(
        t.received_frames(),
        [(47_000, 47_000, true), (47_004, 47_004, false)]
    );
    let data = t
        .receiver
        .events
        .iter()
        .find_map(|(_, event)| match event {
            Event::MediaData(data) if **data.seq_range.start() == 47_004 => Some(data),
            _ => None,
        })
        .unwrap();
    assert_eq!(data.data.as_ref(), &[0, 0, 0, 1, 0x61, 0x44]);
    t.advance_to(deadline + Duration::from_millis(100))?;
    assert_eq!(t.received_frames().len(), 2);
    assert_ne!(t.receiver.last_timeout_reason(), Reason::ReceiveReorder);
    Ok(())
}

/// Test NACK/RTX recovery fills the gap before expiry and preserves frame continuity.
#[test]
fn video_reorder_timeout_rtx_recovers() -> Result<(), RtcError> {
    let timeout = Duration::from_millis(300);
    let mut t = VideoTest::new(Rtc::builder(), true)?;
    t.send_vp8_frame(47_000)?;
    t.advance_to(t.now + Duration::from_millis(100))?;
    t.drop_original = Some(47_001);
    t.send_vp8_frame(47_001)?;
    assert_eq!(t.dropped, 1);
    t.advance_to(t.now + Duration::from_millis(100))?;
    t.send_vp8_frame(47_002)?;
    let deadline = t.now + timeout;
    t.advance_to(deadline - Duration::from_millis(1))?;
    assert_eq!(
        t.received_frames(),
        [
            (47_000, 47_000, true),
            (47_001, 47_001, true),
            (47_002, 47_002, true)
        ]
    );
    assert!(
        t.receiver
            .events
            .iter()
            .any(|(_, e)| matches!(e.as_raw_packet(),
        Some(RawPacket::RtcpTx(Rtcp::Nack(n))) if n.reports.iter().any(|r| r.pid == 47_001)))
    );
    assert!(
        t.sender
            .events
            .iter()
            .any(|(_, e)| matches!(e.as_raw_packet(),
        Some(RawPacket::RtcpRx(Rtcp::Nack(n))) if n.reports.iter().any(|r| r.pid == 47_001)))
    );
    let repair_pt = t.sender.params_vp8().resend().unwrap();
    assert!(
        t.receiver
            .events
            .iter()
            .any(|(_, e)| matches!(e.as_raw_packet(),
        Some(RawPacket::RtpRx(header, payload)) if header.payload_type == repair_pt
            && payload.get(..2) == Some(&47_001u16.to_be_bytes()[..])))
    );
    t.advance_to(deadline + Duration::from_millis(100))?;
    assert_eq!(t.received_frames().len(), 3);
    Ok(())
}

/// RTX received in a later reporting interval can make that interval's loss negative.
#[test]
fn video_receiver_report_loss_after_delayed_rtx() -> Result<(), RtcError> {
    let mut t = VideoTest::new(Rtc::builder().set_rtp_mode(true), true)?;
    t.defer_rtx = true;

    // Receive 118 of 120 packets and hold the sender's repairs until after the RR.
    for seq in 47_000..47_120 {
        if [47_110, 47_111].contains(&seq) {
            t.drop_original = Some(seq as u16);
        }
        t.send_vp8_frame(seq)?;
    }
    assert_eq!(t.dropped, 2);

    // Inspect the RR received by the sender, exercising RTCP serialization too.
    let next_report = |t: &mut VideoTest| -> Result<_, RtcError> {
        let deadline = t.now + Duration::from_secs(5);
        loop {
            t.advance_to(t.now + Duration::from_millis(10))?;
            if let Some(report) =
                t.sender
                    .events
                    .iter()
                    .find_map(|(_, event)| match event.as_raw_packet() {
                        Some(RawPacket::RtcpRx(Rtcp::ReceiverReport(rr))) => {
                            rr.reports.iter().find(|r| *r.ssrc == 1337).copied()
                        }
                        _ => None,
                    })
            {
                return Ok(report);
            }
            assert!(t.now < deadline, "sender did not receive a receiver report");
        }
    };

    let first = next_report(&mut t)?;
    assert_eq!(first.max_seq, 47_119);
    assert_eq!(first.packets_lost, 2);
    assert_eq!(first.fraction_lost, 4);
    assert!(!t.deferred_rtx.is_empty(), "sender must retransmit via RTX");
    t.sender.events.clear();

    t.defer_rtx = false;
    for packet in std::mem::take(&mut t.deferred_rtx) {
        t.receiver.handle_input(Input::Receive(
            t.now,
            Receive {
                proto: packet.proto,
                source: packet.source,
                destination: packet.destination,
                contents: packet.contents.as_slice().try_into()?,
            },
        ))?;
        t.flush()?;
    }

    // The two repairs plus 120 new packets mean 122 received versus 120 expected.
    for seq in 47_120..47_240 {
        t.send_vp8_frame(seq)?;
    }
    let mut received: Vec<_> = t
        .receiver
        .events
        .iter()
        .filter_map(|(_, event)| match event {
            Event::RtpPacket(packet) => Some(*packet.seq_no),
            _ => None,
        })
        .collect();
    received.sort_unstable();
    assert_eq!(received, (47_000..47_240).collect::<Vec<_>>());

    let second = next_report(&mut t)?;
    assert_eq!(second.max_seq, 47_239);
    assert_eq!(second.packets_lost, 0);
    assert_eq!(
        second.fraction_lost, 0,
        "negative interval loss must be zero"
    );
    Ok(())
}

/// Test RTX arriving after timeout release cannot resurrect an older frame.
#[test]
fn video_reorder_timeout_rtx_after_release() -> Result<(), RtcError> {
    let mut t = VideoTest::new(Rtc::builder(), true)?;
    t.defer_rtx = true;
    t.send_vp8_frame(47_000)?;
    t.advance_to(t.now + Duration::from_millis(100))?;
    t.drop_original = Some(47_001);
    t.send_vp8_frame(47_001)?;
    assert_eq!(t.dropped, 1);
    t.advance_to(t.now + Duration::from_millis(100))?;
    t.send_vp8_frame(47_002)?;
    let mut last_nack = None;
    for _ in 0..200 {
        let requests: Vec<_> = t
            .receiver
            .events
            .iter()
            .filter_map(|(at, e)| {
                matches!(e.as_raw_packet(), Some(RawPacket::RtcpTx(Rtcp::Nack(n)))
                if n.reports.iter().any(|r| r.pid == 47_001))
                .then_some(*at)
            })
            .collect();
        assert_eq!(t.received_frames(), [(47_000, 47_000, true)]);
        if requests.len() == 5 {
            last_nack = requests.last().copied();
            break;
        }
        assert!(requests.len() < 5);
        t.advance_to(t.now + Duration::from_millis(10))?;
    }
    let deadline = last_nack.expect("five NACK attempts") + Duration::from_millis(300);
    t.advance_to(deadline - Duration::from_nanos(1))?;
    assert_eq!(t.received_frames(), [(47_000, 47_000, true)]);
    assert!(
        !t.deferred_rtx.is_empty(),
        "hold genuine sender-generated RTX"
    );
    assert!(
        t.receiver
            .events
            .iter()
            .any(|(_, e)| matches!(e.as_raw_packet(),
        Some(RawPacket::RtcpTx(Rtcp::Nack(n))) if n.reports.iter().any(|r| r.pid == 47_001)))
    );
    assert!(
        t.sender
            .events
            .iter()
            .any(|(_, e)| matches!(e.as_raw_packet(),
        Some(RawPacket::RtcpRx(Rtcp::Nack(n))) if n.reports.iter().any(|r| r.pid == 47_001)))
    );
    let repair_pt = t.repair_pt.unwrap();
    let is_repair = |e: &Event| {
        matches!(e.as_raw_packet(),
        Some(RawPacket::RtpRx(header, payload)) if header.payload_type == repair_pt
            && *header.ssrc == 1338
            && payload.get(..2) == Some(&47_001u16.to_be_bytes()[..]))
    };
    assert!(!t.receiver.events.iter().any(|(_, e)| is_repair(e)));
    assert_eq!(t.receiver.last, deadline);
    assert_eq!(t.receiver.last_timeout_reason(), Reason::ReceiveReorder);
    t.tick(deadline)?;
    let released = [(47_000, 47_000, true), (47_002, 47_002, false)];
    assert_eq!(t.received_frames(), released);

    t.advance_to(deadline + Duration::from_millis(50))?;
    t.defer_rtx = false;
    for packet in std::mem::take(&mut t.deferred_rtx) {
        t.receiver.handle_input(Input::Receive(
            t.now,
            Receive {
                proto: packet.proto,
                source: packet.source,
                destination: packet.destination,
                contents: packet.contents.as_slice().try_into()?,
            },
        ))?;
    }
    t.flush()?;
    assert!(
        t.receiver
            .events
            .iter()
            .any(|(at, e)| *at > deadline && is_repair(e))
    );
    assert_eq!(
        t.received_frames(),
        released,
        "late RTX must not resurrect 47001"
    );
    t.send_vp8_frame(47_003)?;
    t.advance_to(deadline + Duration::from_millis(100))?;
    assert_eq!(
        t.received_frames(),
        [
            (47_000, 47_000, true),
            (47_002, 47_002, false),
            (47_003, 47_003, true)
        ]
    );
    assert_ne!(t.receiver.last_timeout_reason(), Reason::ReceiveReorder);
    Ok(())
}

/// Audio's timeout cannot disable or change automatic video recovery.
#[test]
fn video_recovery_is_independent_of_audio_timeout() -> Result<(), RtcError> {
    for timeout in [None, Some(Duration::ZERO), Some(Duration::from_secs(5))] {
        let mut t = VideoTest::new(Rtc::builder().set_reordering_timeout_audio(timeout), false)?;
        t.send_vp8_frame(47_000)?;
        t.advance_to(t.now + Duration::from_millis(100))?;
        t.write(1337.into(), 47_001, 100, &[0x10, 0, 0], false)?;
        t.write(1337.into(), 47_003, 200, &[0x10, 0, 0], false)?;
        let deadline = t.now + Duration::from_millis(300);
        t.advance_to(t.now + Duration::from_millis(100))?;
        assert_eq!(t.received_frames(), [(47_000, 47_000, true)]);
        t.write(1337.into(), 47_004, 200, &[0x00, 0], true)?;
        t.advance_to(deadline - Duration::from_nanos(1))?;
        assert_eq!(t.received_frames(), [(47_000, 47_000, true)]);
        t.advance_to(deadline)?;
        assert_eq!(
            t.received_frames(),
            [(47_000, 47_000, true), (47_003, 47_004, false)]
        );
    }
    Ok(())
}

/// Test the earliest video deadline wins across media, regardless of iteration order.
#[test]
fn video_reorder_timeout_earliest_across_media() -> Result<(), RtcError> {
    let mut t = VideoTest::new(Rtc::builder(), false)?;
    t.add_video("other".into(), 2337.into(), false);
    t.send_vp8_frame(47_000)?;
    t.write(2337.into(), 48_000, 1000, &[0x10, 0, 0], true)?;
    let base = t.now;
    t.advance_to(base + Duration::from_millis(100))?;
    // The second media has the earlier deadline, independent of iteration order.
    t.write(2337.into(), 48_002, 2000, &[0x10, 0, 0], true)?;
    t.advance_to(base + Duration::from_millis(150))?;
    t.send_vp8_frame(47_002)?;
    t.advance_to(base + Duration::from_millis(399))?;
    assert_eq!(t.received_frames().len(), 2);
    assert_eq!(t.receiver.last, base + Duration::from_millis(400));
    t.tick(t.receiver.last)?;
    assert_eq!(t.received_frames().last(), Some(&(48_002, 48_002, false)));
    t.advance_to(base + Duration::from_millis(449))?;
    assert_eq!(t.receiver.last, base + Duration::from_millis(450));
    t.tick(t.receiver.last)?;
    assert_eq!(t.received_frames().last(), Some(&(47_002, 47_002, false)));
    assert_eq!(t.received_frames().len(), 4);
    Ok(())
}

/// A frame can start before a stream pause and finish after its resume.
#[test]
fn video_frame_completes_across_pause() -> Result<(), RtcError> {
    for pause_ms in [100, 500] {
        let mut t = VideoTest::new(
            Rtc::builder().set_pause_threshold(Duration::from_millis(pause_ms)),
            false,
        )?;
        t.write(1337.into(), 47_000, 1000, &[0x10, 0, 0], false)?;
        assert!(t.received_frames().is_empty());
        t.advance_to(t.now + Duration::from_millis(650))?;
        assert!(
            t.receiver.events.iter().any(|(_, event)| {
                matches!(event, Event::StreamPaused(paused) if paused.paused)
            })
        );
        t.write(1337.into(), 47_001, 1000, &[0x00, 0], true)?;
        assert_eq!(t.received_frames(), [(47_000, 47_001, true)]);
        assert!(
            t.receiver.events.iter().any(|(_, event)| {
                matches!(event, Event::StreamPaused(paused) if !paused.paused)
            })
        );
    }
    Ok(())
}

/// A lost tail must not keep a fresh frame hidden for an entire reordering window
/// after the receiver has reported a pause.
#[test]
fn video_new_frame_after_paused_incomplete_frame_makes_progress() -> Result<(), RtcError> {
    let mut t = VideoTest::new(
        Rtc::builder().set_pause_threshold(Duration::from_millis(500)),
        false,
    )?;
    t.write(1337.into(), 46_999, 500, &[0x10, 0, 0], true)?;
    assert_eq!(t.received_frames(), [(46_999, 46_999, true)]);
    t.write(1337.into(), 47_000, 1000, &[0x10, 0, 0], false)?;
    t.advance_to(t.now + Duration::from_millis(650))?;
    assert!(
        t.receiver
            .events
            .iter()
            .any(|(_, event)| { matches!(event, Event::StreamPaused(paused) if paused.paused) })
    );

    // Packet 47_001 (the old frame's tail) was lost. The sender resumes
    // with a complete new frame, which should be delivered as noncontiguous
    // within a bounded interval even if no more packets arrive.
    t.write(1337.into(), 47_002, 2000, &[0x10, 0, 0], true)?;
    t.advance_to(t.now + Duration::from_millis(2500))?;
    assert_eq!(
        t.received_frames(),
        [(46_999, 46_999, true), (47_002, 47_002, false)]
    );
    Ok(())
}

/// A new frame after a pause must not implicitly complete the old frame's missing tail.
#[test]
fn video_new_frame_start_drops_paused_incomplete_frame() -> Result<(), RtcError> {
    let mut t = VideoTest::new(
        Rtc::builder().set_pause_threshold(Duration::from_millis(500)),
        false,
    )?;
    t.write(1337.into(), 46_999, 500, &[0x10, 0, 0], true)?;
    t.write(1337.into(), 47_000, 1000, &[0x10, 0, 0], false)?;
    t.advance_to(t.now + Duration::from_millis(650))?;

    // Same SSRC and consecutive sequence numbers, but a new VP8 frame starts.
    t.write(1337.into(), 47_001, 2000, &[0x10, 0, 0], false)?;
    assert_eq!(t.received_frames(), [(46_999, 46_999, true)]);
    t.write(1337.into(), 47_002, 2000, &[0x00, 0], true)?;
    t.advance_to(t.now + Duration::from_millis(2200))?;
    assert_eq!(
        t.received_frames(),
        [(46_999, 46_999, true), (47_001, 47_002, false)]
    );
    Ok(())
}

/// A restarted sender can keep its SSRC while starting its picture IDs over.
#[test]
fn video_same_ssrc_restart_after_pause_accepts_new_keyframe() -> Result<(), RtcError> {
    for previous_picture_id in [40, 1] {
        let mut t = VideoTest::new(
            Rtc::builder().set_pause_threshold(Duration::from_millis(500)),
            false,
        )?;
        let vp8_keyframe = |picture_id: u8| [0x90, 0xe0, picture_id, picture_id, 0, 0, 0, 0];
        t.write(
            1337.into(),
            47_000,
            1000,
            &vp8_keyframe(previous_picture_id),
            true,
        )?;
        assert_eq!(t.received_frames(), [(47_000, 47_000, true)]);
        t.advance_to(t.now + Duration::from_millis(650))?;
        assert!(
            t.receiver
                .events
                .iter()
                .any(|(_, event)| matches!(event, Event::StreamPaused(p) if p.paused))
        );

        t.write(1337.into(), 47_001, 2000, &vp8_keyframe(1), true)?;
        assert_eq!(
            t.received_frames(),
            [(47_000, 47_000, true), (47_001, 47_001, false)],
            "keyframe after PictureID {previous_picture_id} was not emitted"
        );
    }
    Ok(())
}

/// Video recovery leaves RTP-mode packet delivery unchanged.
#[test]
fn video_reorder_timeout_does_not_change_rtp_mode() -> Result<(), RtcError> {
    let mut t = VideoTest::new(Rtc::builder().set_rtp_mode(true), false)?;
    t.send_vp8_frame(47_000)?;
    t.send_vp8_frame(47_002)?;
    assert!(t.received_frames().is_empty());
    let seqs: Vec<_> = t
        .receiver
        .events
        .iter()
        .filter_map(|(_, e)| match e {
            Event::RtpPacket(p) => Some(*p.seq_no),
            _ => None,
        })
        .collect();
    assert_eq!(seqs, [47_000, 47_002]);
    assert_ne!(t.receiver.last_timeout_reason(), Reason::ReceiveReorder);
    Ok(())
}

/// Video recovery does not change audio's separate timeout.
#[test]
fn video_reorder_timeout_does_not_change_audio() -> Result<(), RtcError> {
    let mut t = VideoTest::new(Rtc::builder(), false)?;
    let mid = "audio".into();
    let ssrc = 3337.into();
    let pt = t.sender.params_opus().pt();
    t.sender.direct_api().declare_media(mid, MediaKind::Audio);
    t.sender
        .direct_api()
        .declare_stream_tx(ssrc, None, mid, None);
    t.receiver.direct_api().declare_media(mid, MediaKind::Audio);
    t.receiver
        .direct_api()
        .expect_stream_rx(ssrc, None, mid, None);
    for seq in [100u64, 102] {
        t.sender.direct_api().stream_tx(&ssrc).unwrap().write_rtp(
            RtpWrite::new(pt, seq.into(), seq as u32 * 960, t.now, [0xf8, 0xff, 0xfe]).marker(true),
        );
        t.tick(t.now)?;
    }
    t.advance_to(t.now + Duration::from_millis(500))?;
    assert_eq!(t.received_frames(), [(100, 100, true)]);
    assert_ne!(t.receiver.last_timeout_reason(), Reason::ReceiveReorder);
    Ok(())
}
