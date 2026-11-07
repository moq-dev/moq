use std::net::Ipv4Addr;
use std::time::{Duration, Instant};
use str0m::Rtc;
use str0m::format::Codec;
use str0m::media::{Direction, MediaData, MediaKind};
use str0m::rtp::RtpWrite;
use str0m::{Candidate, Event, RtcError};
use tracing::info_span;

mod common;
use common::{Peer, TestRtc, init_crypto_default, init_log, progress};

#[test]
pub fn contiguous_all_the_way() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let output = Server::with_vp8_input()
        .timeout(Duration::from_secs(10))
        .get_output()?;
    let mut count = 0;

    // Contiguous all the way through.
    for data in output {
        assert!(data.contiguous);
        count += 1;
    }

    // We have 3 continuations: 104 - 3 == 101
    assert_eq!(count, 101);

    Ok(())
}

#[test]
pub fn not_contiguous() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let output = Server::with_vp8_input()
        .skip_packet(14337)
        .timeout(Duration::from_secs(5))
        .get_output()?;
    let mut count = 0;

    // Contiguous all the way through.
    for data in output {
        count += 1;
        // We dropped packet 14337, which means its dependendant 14338 is not
        // emitted, and 14339 is emitted and marked as discontinuous.
        let assume_contiguous = !data.seq_range.contains(&14339.into());
        assert_eq!(assume_contiguous, data.contiguous);
    }

    // assert!(false);
    // We have 3 continuations, 2 missing packet (14337 14338)
    // 104 - 3 - 2  == 99
    assert_eq!(count, 99);

    Ok(())
}

#[test]
pub fn vp9_contiguous_all_the_way() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let output = Server::with_vp9_input().get_output()?;
    let mut count = 0;

    // Contiguous all the way through.
    for data in output {
        assert!(data.contiguous);
        count += 1;
    }

    // The last packet is never flushed out because the depacketizer wants
    // to see the next packet before releasing.
    // We have one last packet missing: 16 - 1 == 15
    assert_eq!(count, 15);

    Ok(())
}

#[test]
pub fn vp9_not_contiguous() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let output = Server::with_vp9_input().skip_packet(30952).get_output()?;
    let mut count = 0;

    // Contiguous all the way through.
    for data in output {
        count += 1;
        // We dropped packet 19365 and next packet is emitted and marked as discontinuous.
        let assume_contiguous = !data.seq_range.contains(&30953.into());
        assert_eq!(assume_contiguous, data.contiguous);
    }

    // assert!(false);
    // We 1 missing packet 30952, and one last
    // packet missing: 16 - 1 - 1 == 14
    assert_eq!(count, 14);

    Ok(())
}

struct Server {
    codec: Codec,
    input_data: common::PcapData,
    skip_packet: Option<u16>,
    timeout: Option<Duration>,
}

impl Server {
    fn with_vp8_input() -> Self {
        Self::new(Codec::Vp8, common::vp8_data())
    }

    fn with_vp9_input() -> Self {
        Self::new(Codec::Vp9, common::vp9_contiguous_data())
    }

    fn new(codec: Codec, input_data: common::PcapData) -> Self {
        Self {
            codec,
            input_data,
            skip_packet: None,
            timeout: None,
        }
    }

    fn skip_packet(mut self, packet: u16) -> Self {
        self.skip_packet = Some(packet);
        self
    }

    fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    fn get_output(self) -> Result<Vec<MediaData>, RtcError> {
        let mut l = TestRtc::new(Peer::Left);

        // We need to lower the default reordering buffer size, or we won't make it
        // past the dropped packet.
        let rtc_r = Rtc::builder()
            .set_reordering_size_video(5)
            .build(Instant::now());

        let mut r = TestRtc::new_with_rtc(info_span!("R"), rtc_r);

        l.add_local_candidate(Candidate::host(
            (Ipv4Addr::new(1, 1, 1, 1), 1000).into(),
            "udp",
        )?);
        r.add_local_candidate(Candidate::host(
            (Ipv4Addr::new(2, 2, 2, 2), 2000).into(),
            "udp",
        )?);

        // The change is on the L (sending side) with Direction::SendRecv.
        let mut change = l.sdp_api();
        let mid = change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
        let (offer, pending) = change.apply().unwrap();

        let answer = r.rtc.sdp_api().accept_offer(offer)?;
        l.rtc.sdp_api().accept_answer(pending, answer)?;

        loop {
            if l.is_connected() || r.is_connected() {
                break;
            }
            progress(&mut l, &mut r)?;
        }

        let max = l.last.max(r.last);
        l.last = max;
        r.last = max;

        let params = match self.codec {
            Codec::Vp8 => l.params_vp8(),
            Codec::Vp9 => l.params_vp9(),
            _ => unimplemented!(),
        };
        assert_eq!(params.spec().codec, self.codec);

        let pt = params.pt();

        for (relative, header, payload) in self.input_data {
            // Drop a random packet in the middle.
            if Some(header.sequence_number) == self.skip_packet {
                continue;
            }

            // Keep RTC time progressed to be "in sync" with the test data.
            while (l.last - max) < relative {
                progress(&mut l, &mut r)?;
            }

            let absolute = max + relative;

            let mut direct = l.direct_api();
            let tx = direct.stream_tx_by_mid(mid, None).unwrap();
            tx.write_rtp(
                RtpWrite::new(
                    pt,
                    header.sequence_number(None),
                    header.timestamp,
                    absolute,
                    payload,
                )
                .marker(header.marker)
                .ext_vals(header.ext_vals)
                .nackable(true),
            );

            progress(&mut l, &mut r)?;

            if let Some(duration) = self.timeout {
                if l.duration() > duration {
                    break;
                }
            }
        }

        // Drain any remaining packets from the pacer
        progress(&mut l, &mut r)?;

        let events = r
            .events
            .into_iter()
            .filter_map(|(_, e)| {
                if let Event::MediaData(d) = e {
                    Some(d)
                } else {
                    None
                }
            })
            .collect();

        Ok(events)
    }
}
