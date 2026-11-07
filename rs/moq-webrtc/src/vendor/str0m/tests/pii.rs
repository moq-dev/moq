//! If this test fails, it indicates that an IP address or other personally
//! identifiable information (PII) was logged. Ensure that sensitive values
//! checked by this test are wrapped using the `Pii` wrapper.
//! Run this test with:
//! ```shell
//! cargo test --test pii --features pii
//! ```

mod common;
#[cfg(feature = "pii")]
mod pii_log_redaction {
    use common::{connect_l_r, init_crypto_default, progress};
    use std::collections::VecDeque;
    use std::time::Duration;

    use str0m::format::Codec;
    use str0m::media::MediaKind;
    use str0m::rtp::{ExtensionValues, RtpWrite, Ssrc};
    use str0m::{Event, RtcError};

    use super::*;
    use regex::Regex;
    use std::sync::Once;
    use tracing::{Event as TracingEvent, Subscriber};
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::{Layer, Registry, layer::Context};

    static INIT: Once = Once::new();

    struct AssertNoIpLayer {
        ipv4: Regex,
        ipv6: Regex,
    }

    impl<S: Subscriber> Layer<S> for AssertNoIpLayer {
        fn on_event(&self, event: &TracingEvent<'_>, _ctx: Context<'_, S>) {
            let mut visitor = StringVisitor::default();
            event.record(&mut visitor);
            let msg = visitor.0;
            if self.ipv4.is_match(&msg) {
                panic!("IPv4 address found in log: {}", msg);
            }
            if self.ipv6.is_match(&msg) {
                panic!("IPv6 address found in log: {}", msg);
            }
        }
    }

    #[derive(Default)]
    struct StringVisitor(String);
    impl tracing::field::Visit for StringVisitor {
        fn record_debug(&mut self, _field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            use std::fmt::Write;
            let _ = write!(&mut self.0, "{:?}", value);
        }
        fn record_str(&mut self, _field: &tracing::field::Field, value: &str) {
            self.0.push_str(value);
        }
    }

    fn install_assert_no_ip_layer() {
        INIT.call_once(|| {
            let ipv4 = Regex::new(r"\b(?:[0-9]{1,3}\.){3}[0-9]{1,3}\b").unwrap();
            let ipv6 = Regex::new(r"\b([0-9a-fA-F]{0,4}:){2,7}[0-9a-fA-F]{0,4}\b").unwrap();
            let layer = AssertNoIpLayer { ipv4, ipv6 }
                .with_filter(tracing_subscriber::filter::LevelFilter::DEBUG);
            let subscriber = Registry::default().with(layer);
            tracing::subscriber::set_global_default(subscriber).expect("set global subscriber");
        });
    }

    #[test]
    fn pii_test() -> Result<(), RtcError> {
        install_assert_no_ip_layer();
        init_crypto_default();

        let (mut l, mut r) = connect_l_r();

        let mid = "aud".into();

        // In this example we are using MID only (no RID) to identify the incoming media.
        let ssrc_tx: Ssrc = 42.into();

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
                        RtpWrite::new(pt, seq_no, time, wallclock, packet).ext_vals(exts),
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
                if let Event::RtpPacket(v) = e {
                    Some(v)
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(media.len(), 3);

        assert!(l.media(mid).is_some());
        assert!(l.direct_api().stream_tx_by_mid(mid, None).is_some());
        l.direct_api().remove_media(mid);
        assert!(l.media(mid).is_none());
        assert!(l.direct_api().stream_tx_by_mid(mid, None).is_none());

        assert!(r.media(mid).is_some());
        assert!(r.direct_api().stream_rx_by_mid(mid, None).is_some());
        r.direct_api().remove_media(mid);
        assert!(r.media(mid).is_none());
        assert!(r.direct_api().stream_rx_by_mid(mid, None).is_none());

        Ok(())
    }
}
