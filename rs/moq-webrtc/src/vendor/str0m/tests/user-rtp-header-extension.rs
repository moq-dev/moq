use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use str0m::Rtc;
use str0m::change::SdpOffer;
use str0m::format::Codec;
use str0m::media::{Direction, MediaKind};
use str0m::rtp::Extension;
use str0m::rtp::ExtensionSerializer;
use str0m::rtp::ExtensionValues;
use str0m::{Event, RtcError};
use tracing::info_span;

mod common;
use common::{TestRtc, init_crypto_default, init_log, progress};

#[test]
pub fn user_rtp_header_extension() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    #[derive(Debug, PartialEq, Eq)]
    struct MyValue(u16);

    #[derive(Debug)]
    struct MyValueSerializer;

    impl ExtensionSerializer for MyValueSerializer {
        fn write_to(&self, buf: &mut [u8], ev: &ExtensionValues) -> usize {
            // Does output have space?
            if buf.len() < 2 {
                return 0;
            }

            // Is there a value set?
            let Some(my_value) = ev.user_values.get::<MyValue>() else {
                return 0;
            };

            // u16 is 2 bytes
            buf[..2].copy_from_slice(&my_value.0.to_be_bytes());

            2
        }

        fn parse_value(&self, buf: &[u8], ev: &mut ExtensionValues) -> bool {
            // Is buffer big enough to hold the value?
            if buf.len() < 2 {
                return false;
            }

            let v = u16::from_be_bytes([buf[0], buf[1]]);
            let my_value = MyValue(v);

            // Save parsed value
            ev.user_values.set(my_value);

            true
        }

        fn is_audio(&self) -> bool {
            true
        }

        fn is_video(&self) -> bool {
            true
        }
    }

    let user_ext = Extension::with_serializer("http://my-special-extension", MyValueSerializer);

    // Both L and R must have the uri + serializer configured.
    let now = Instant::now();
    let rtc_l = Rtc::builder()
        //
        .set_extension(12, user_ext.clone())
        .build(now);
    let rtc_r = Rtc::builder()
        //
        .set_extension(12, user_ext)
        .build(now);

    let mut l = TestRtc::new_with_rtc(info_span!("L"), rtc_l);
    let mut r = TestRtc::new_with_rtc(info_span!("R"), rtc_r);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    // The change is on the L (sending side) with Direction::SendRecv.
    let mut change = l.sdp_api();
    let mid = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
    let (offer, pending) = change.apply().unwrap();
    let offer_str = offer.to_sdp_string();
    let offer_parsed =
        SdpOffer::from_sdp_string(&offer_str).expect("Should parse offer from string");
    let answer = r.rtc.sdp_api().accept_offer(offer_parsed)?;
    l.rtc.sdp_api().accept_answer(pending, answer)?;

    // Verify that the extension is negotiated.
    let ext_l = l.media(mid).unwrap().remote_extmap();
    assert_eq!(
        ext_l.lookup(12).map(|e| e.as_uri()),
        Some("http://my-special-extension")
    );

    let ext_r = r.media(mid).unwrap().remote_extmap();
    assert_eq!(
        ext_r.lookup(12).map(|e| e.as_uri()),
        Some("http://my-special-extension")
    );

    loop {
        if l.is_connected() || r.is_connected() {
            break;
        }
        progress(&mut l, &mut r)?;
    }

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_opus();
    assert_eq!(params.spec().codec, Codec::Opus);
    let pt = params.pt();

    let data_a = vec![1_u8; 80];

    loop {
        let wallclock = l.start + l.duration();
        let time = l.duration().into();

        l.writer(mid)
            .unwrap()
            // Set my bespoke RTP header value.
            .user_extension_value(MyValue(42))
            .write(pt, wallclock, time, data_a.clone())?;

        progress(&mut l, &mut r)?;

        if l.duration() > Duration::from_secs(3) {
            break;
        }
    }

    let iter = r.events.iter().filter_map(|(_, e)| {
        if let Event::MediaData(d) = e {
            Some(d)
        } else {
            None
        }
    });

    // Assert every media write got the value through.
    for data in iter {
        let v = data.ext_vals.user_values.get::<MyValue>();
        assert_eq!(v, Some(&MyValue(42)));
    }

    Ok(())
}

#[test]
pub fn user_rtp_header_extension_two_byte_form() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    #[derive(Debug, PartialEq, Eq, Clone)]
    struct MyValue(Vec<u8>);

    #[derive(Debug)]
    struct MyValueSerializer;

    impl ExtensionSerializer for MyValueSerializer {
        fn write_to(&self, buf: &mut [u8], ev: &ExtensionValues) -> usize {
            let Some(my_value) = ev.user_values.get::<MyValue>() else {
                return 0;
            };

            let my_len = my_value.0.len();
            if buf.len() < my_len {
                return 0;
            }

            // u16 is 2 bytes
            buf[..my_len].copy_from_slice(&my_value.0);

            my_len
        }

        fn parse_value(&self, buf: &[u8], ev: &mut ExtensionValues) -> bool {
            let my_value = MyValue(buf.to_vec());

            ev.user_values.set(my_value);

            true
        }

        fn is_audio(&self) -> bool {
            true
        }

        fn is_video(&self) -> bool {
            true
        }

        fn requires_two_byte_form(&self, _ev: &ExtensionValues) -> bool {
            true
        }
    }

    let user_ext = Extension::with_serializer("http://my-special-extension", MyValueSerializer);

    // Both L and R must have the uri + serializer configured.
    let now = Instant::now();
    let rtc_l = Rtc::builder()
        //
        .set_extension(12, user_ext.clone())
        .build(now);
    let rtc_r = Rtc::builder()
        //
        .set_extension(12, user_ext)
        .build(now);

    let mut l = TestRtc::new_with_rtc(info_span!("L"), rtc_l);
    let mut r = TestRtc::new_with_rtc(info_span!("R"), rtc_r);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    // The change is on the L (sending side) with Direction::SendRecv.
    let mut change = l.sdp_api();
    let mid = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
    let (offer, pending) = change.apply().unwrap();

    let answer = r.rtc.sdp_api().accept_offer(offer)?;
    l.rtc.sdp_api().accept_answer(pending, answer)?;

    // Verify that the extension is negotiated.
    let ext_l = l.media(mid).unwrap().remote_extmap();
    assert_eq!(
        ext_l.lookup(12).map(|e| e.as_uri()),
        Some("http://my-special-extension")
    );

    let ext_r = r.media(mid).unwrap().remote_extmap();
    assert_eq!(
        ext_r.lookup(12).map(|e| e.as_uri()),
        Some("http://my-special-extension")
    );

    loop {
        if l.is_connected() || r.is_connected() {
            break;
        }
        progress(&mut l, &mut r)?;
    }

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_opus();
    assert_eq!(params.spec().codec, Codec::Opus);
    let pt = params.pt();

    let data_a = vec![1_u8; 80];

    let my_value = MyValue((0..100u8).collect());
    loop {
        let wallclock = l.start + l.duration();
        let time = l.duration().into();

        l.writer(mid)
            .unwrap()
            // Set my bespoke RTP header value.
            .user_extension_value(my_value.clone())
            .write(pt, wallclock, time, data_a.clone())?;

        progress(&mut l, &mut r)?;

        if l.duration() > Duration::from_secs(3) {
            break;
        }
    }

    let datas = r.events.iter().filter_map(|(_, e)| {
        if let Event::MediaData(d) = e {
            Some(d)
        } else {
            None
        }
    });

    // Assert every media write got the value through.
    let mut empty = true;
    for data in datas {
        empty = false;
        let v = data.ext_vals.user_values.get::<MyValue>();
        assert_eq!(v, Some(&my_value));
    }
    assert!(!empty);

    Ok(())
}
