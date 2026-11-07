use std::time::Duration;

use str0m::format::Codec;
use str0m::media::MediaKind;
use str0m::rtp::{ExtensionValues, RtpWrite, Ssrc};
use str0m::{Input, RtcError};

mod common;
use common::{connect_l_r, init_crypto_default, init_log};

/// This test triggers the panic in `Session::poll_packet` where the pacer
/// holds a reference to a MidRid whose media has been removed.
///
/// Sequence:
/// 1. Set up a connected pair with a media + stream via direct API
/// 2. Write an RTP packet so the stream's send queue is non-empty
/// 3. Call handle_input(Timeout) so session.handle_timeout populates the pacer
/// 4. Remove the media via direct_api().remove_media() — pacer still references the mid
/// 5. Call poll_output() — poll_packet asks pacer for next mid, finds it missing → panic
#[test]
pub fn remove_media_while_pacer_queued() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let (mut l, mut r) = connect_l_r();

    let mid = "aud".into();
    let ssrc_tx: Ssrc = 42.into();

    // Declare media and stream on sender
    l.direct_api().declare_media(mid, MediaKind::Audio);
    l.direct_api().declare_stream_tx(ssrc_tx, None, mid, None);

    // Declare matching media on receiver
    r.direct_api().declare_media(mid, MediaKind::Audio);

    // Sync time
    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_opus();
    assert_eq!(params.spec().codec, Codec::Opus);
    let pt = params.pt();

    // Write an RTP packet so the stream's send queue has data
    let wallclock = l.start + l.duration();
    {
        let mut direct = l.direct_api();
        let stream = direct.stream_tx(&ssrc_tx).unwrap();

        let exts = ExtensionValues {
            audio_level: Some(-42),
            voice_activity: Some(false),
            ..Default::default()
        };

        stream.write_rtp(
            RtpWrite::new(
                pt,
                47_000.into(),
                47_000_000,
                wallclock,
                [0x1, 0x2, 0x3, 0x4],
            )
            .ext_vals(exts)
            .nackable(true),
        );
    }

    // Advance time slightly so the timeout fires
    let now = l.last + Duration::from_millis(10);
    l.last = now;

    // Trigger handle_timeout which calls update_queue_state → pacer captures the MidRid
    l.handle_input(Input::Timeout(now))?;

    // Now remove the media — the pacer still holds a reference to mid "aud"
    l.direct_api().remove_media(mid);

    // poll_output will call poll_packet → pacer.poll_queue() returns the stale MidRid → panic
    // (The panic: "Pacer pointed to mid {} which doesn't exist")
    let _output = l.poll_output()?;

    Ok(())
}
