use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use netem::NetemConfig;
use str0m::channel::{ChannelConfig, ChannelId};
use str0m::{Event, Input, Output, Rtc, RtcConfig, RtcError};

mod common;
use common::{Peer, TestRtc, connect_l_r, connect_l_r_with_rtc};
use common::{init_crypto_default, init_log, progress};

/// Poll one peer while deliberately withholding all of its network output.
///
/// This models an application that keeps driving its local RTC while packets
/// to the peer are delayed. Public events are returned to the test; transmits
/// are dropped instead of being handed to the remote `TestRtc`.
fn poll_without_delivering_network(
    rtc: &mut TestRtc,
    now: Instant,
) -> Result<Vec<Event>, RtcError> {
    rtc.rtc.handle_input(Input::Timeout(now))?;

    let mut events = vec![];
    loop {
        match rtc.rtc.poll_output()? {
            Output::Event(event) => events.push(event),
            Output::Transmit(_) => {}
            Output::Timeout(_) => return Ok(events),
        }
    }
}

#[test]
pub fn data_channel() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let mut change = l.sdp_api();
    let cid = change.add_channel("My little channel".into());
    change.add_channel("My little channel 2".into());
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

    loop {
        if let Some(mut chan) = l.channel(cid) {
            chan.write(false, "Hello world! ".as_bytes())
                .expect("to write string");
        }

        progress(&mut l, &mut r)?;

        if l.duration() > Duration::from_secs(10) {
            break;
        }
    }

    assert!(r.events.len() > 120);

    Ok(())
}

#[test]
fn data_channel_configured_max_message_size() -> Result<(), RtcError> {
    init_crypto_default();

    for snap in [false, true] {
        for (left_limit, right_limit) in [
            (32 * 1024, 48 * 1024),
            (384 * 1024, 512 * 1024),
            (2 * 1024 * 1024, 3 * 1024 * 1024),
            (0, 384 * 1024),
        ] {
            let now = Instant::now();
            let build = |limit| {
                Rtc::builder()
                    .set_snap_enabled(snap)
                    .set_sctp_max_message_size(limit)
                    .set_sctp_max_buffered_amount(4 * 1024 * 1024)
                    .build(now)
            };
            let mut l = TestRtc::new_with_rtc(tracing::info_span!("L"), build(left_limit));
            let mut r = TestRtc::new_with_rtc(tracing::info_span!("R"), build(right_limit));
            l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
            r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

            let mut change = l.sdp_api();
            let cid = change.add_channel("custom-limit".into());
            let (offer, pending) = change.apply().unwrap();
            let offer_sdp = offer.to_sdp_string();
            assert!(offer_sdp.contains(&format!("a=max-message-size:{left_limit}\r\n")));
            assert_eq!(offer_sdp.contains("a=sctp-init:"), snap);
            let answer = r.sdp_api().accept_offer(offer)?;
            assert!(
                answer
                    .to_sdp_string()
                    .contains(&format!("a=max-message-size:{right_limit}\r\n"))
            );
            l.sdp_api().accept_answer(pending, answer)?;

            while l.channel(cid).is_none() || r.channel(cid).is_none() {
                progress(&mut l, &mut r)?;
                assert!(
                    l.duration() < Duration::from_secs(10),
                    "channel should open"
                );
            }

            let to_right = vec![1; right_limit as usize];
            let to_left = vec![
                2;
                if left_limit == 0 {
                    2 * 1024 * 1024
                } else {
                    left_limit as usize
                }
            ];
            assert!(
                l.channel(cid)
                    .unwrap()
                    .write(true, &vec![0; right_limit as usize + 1])
                    .is_err()
            );
            if left_limit != 0 {
                assert!(
                    r.channel(cid)
                        .unwrap()
                        .write(true, &vec![0; left_limit as usize + 1])
                        .is_err()
                );
            }
            assert!(l.channel(cid).unwrap().write(true, &to_right)?);
            assert!(r.channel(cid).unwrap().write(true, &to_left)?);

            loop {
                progress(&mut l, &mut r)?;
                let left_received = l
                    .events
                    .iter()
                    .any(|(_, e)| matches!(e, Event::ChannelData(data) if data.data == to_left));
                let right_received = r
                    .events
                    .iter()
                    .any(|(_, e)| matches!(e, Event::ChannelData(data) if data.data == to_right));
                if left_received && right_received {
                    break;
                }
                assert!(
                    l.duration() < Duration::from_secs(20),
                    "messages at the configured receive limits should arrive (SNAP: {snap})"
                );
            }
        }
    }
    Ok(())
}

/// Closing a data channel must propagate to the remote peer (via the SCTP
/// stream reset handshake), and once the handshake completes the freed stream
/// id must be reusable by a new in-band channel.
#[test]
pub fn data_channel_close_reopen() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let mut change = l.sdp_api();
    let cid = change.add_channel("churn".into());
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

    // Wait until both sides see the channel open.
    loop {
        progress(&mut l, &mut r)?;

        let l_open = l
            .events
            .iter()
            .any(|(_, e)| matches!(e, Event::ChannelOpen(id, _) if *id == cid));
        let r_open = r
            .events
            .iter()
            .any(|(_, e)| matches!(e, Event::ChannelOpen(_, _)));

        if l_open && r_open {
            break;
        }
        assert!(
            l.duration() < Duration::from_secs(10),
            "first channel should open on both sides"
        );
    }

    let stream_id = l
        .direct_api()
        .sctp_stream_id_by_channel_id(cid)
        .expect("stream id for open channel");

    // Close locally. The reset handshake must inform the remote, which
    // previously never received ChannelClose.
    l.direct_api().close_data_channel(cid);

    loop {
        progress(&mut l, &mut r)?;

        let l_closed = l
            .events
            .iter()
            .any(|(_, e)| matches!(e, Event::ChannelClose(id) if *id == cid));
        let r_closed = r
            .events
            .iter()
            .any(|(_, e)| matches!(e, Event::ChannelClose(_)));

        if l_closed && r_closed {
            break;
        }
        assert!(
            l.duration() < Duration::from_secs(20),
            "both sides should see ChannelClose"
        );
    }

    // The reset handshake finishes with round-trips (reciprocal reset and
    // RECONFIG-RESPONSEs) that carry no public events, so there is nothing to
    // wait on here. Creating the next channel immediately is fine: its stream
    // id allocation happens on a later timeout, by which time the handshake
    // rounds have been ferried through. The stream id assertion below fails
    // loudly if the allocator did not release the id in time.
    let cid2 = l.direct_api().create_data_channel(ChannelConfig {
        label: "churn2".into(),
        ..Default::default()
    });
    assert_ne!(cid, cid2);

    loop {
        progress(&mut l, &mut r)?;

        let l_open = l.events.iter().any(
            |(_, e)| matches!(e, Event::ChannelOpen(id, label) if *id == cid2 && label == "churn2"),
        );
        let r_open = r
            .events
            .iter()
            .any(|(_, e)| matches!(e, Event::ChannelOpen(_, label) if label == "churn2"));

        if l_open && r_open {
            break;
        }
        assert!(
            l.duration() < Duration::from_secs(30),
            "reopened channel should open on both sides"
        );
    }

    // The freed stream id must have been reused, proving the allocator
    // released it when the reset handshake completed.
    assert_eq!(
        l.direct_api().sctp_stream_id_by_channel_id(cid2),
        Some(stream_id),
        "reopened channel should reuse the freed stream id"
    );

    // Data flows on the reopened channel.
    loop {
        if let Some(mut chan) = l.channel(cid2) {
            chan.write(false, b"hello again").expect("write to succeed");
        }

        progress(&mut l, &mut r)?;

        let got_data = r.events.iter().any(
            |(_, e)| matches!(e, Event::ChannelData(d) if d.data.as_slice() == b"hello again"),
        );
        if got_data {
            break;
        }
        assert!(
            l.duration() < Duration::from_secs(40),
            "data should flow on the reopened channel"
        );
    }

    Ok(())
}

#[test]
pub fn negotiated_reuse_waits_for_reset_before_channel_open() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let (mut l, mut r) = connect_l_r();
    let stream_id = 10;
    let config = ChannelConfig {
        label: "old-generation".into(),
        negotiated: Some(stream_id),
        ..Default::default()
    };
    let old_l = l.direct_api().create_data_channel(config.clone());
    let _old_r = r.direct_api().create_data_channel(config);

    loop {
        progress(&mut l, &mut r)?;
        let left_open = l
            .events
            .iter()
            .any(|(_, event)| matches!(event, Event::ChannelOpen(id, _) if *id == old_l));
        let right_open = r
            .events
            .iter()
            .any(|(_, event)| matches!(event, Event::ChannelOpen(_, _)));
        if left_open && right_open {
            break;
        }
        assert!(
            l.duration() < Duration::from_secs(10),
            "negotiated channel should open on both peers"
        );
    }

    l.events.clear();
    l.direct_api().close_data_channel(old_l);

    // Process the local close, but withhold its RE-CONFIG packets. The old
    // SCTP stream therefore still exists and its reset cannot be complete.
    let close_at = l.last;
    let close_events = poll_without_delivering_network(&mut l, close_at)?;
    assert!(
        close_events
            .iter()
            .any(|event| matches!(event, Event::ChannelClose(id) if *id == old_l)),
        "the old local channel should close"
    );

    let replacement = l.direct_api().create_data_channel(ChannelConfig {
        label: "replacement".into(),
        negotiated: Some(stream_id),
        ..Default::default()
    });
    let replacement_events =
        poll_without_delivering_network(&mut l, close_at + Duration::from_millis(1))?;

    assert!(
        !replacement_events
            .iter()
            .any(|event| matches!(event, Event::ChannelOpen(id, _) if *id == replacement)),
        "a negotiated replacement must not open before the old reset completes"
    );

    Ok(())
}

#[test]
pub fn unconfirmed_reset_keeps_stream_id_reserved() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let (mut l, mut r) = connect_l_r();
    let old = l.direct_api().create_data_channel(ChannelConfig::default());

    loop {
        progress(&mut l, &mut r)?;
        if l.events
            .iter()
            .any(|(_, event)| matches!(event, Event::ChannelOpen(id, _) if *id == old))
        {
            break;
        }
        assert!(
            l.duration() < Duration::from_secs(10),
            "first channel should open"
        );
    }

    let old_stream_id = l
        .direct_api()
        .sctp_stream_id_by_channel_id(old)
        .expect("old channel should have a stream ID");
    l.direct_api().close_data_channel(old);

    // Drop all local reset traffic and advance well past the close. No peer response
    // has made the old ID safe during this interval, so it stays reserved.
    let close_at = l.last;
    let close_events = poll_without_delivering_network(&mut l, close_at)?;
    assert!(
        close_events
            .iter()
            .any(|event| matches!(event, Event::ChannelClose(id) if *id == old)),
        "the old local channel should close"
    );
    poll_without_delivering_network(&mut l, close_at + Duration::from_secs(31))?;

    let replacement = l.direct_api().create_data_channel(ChannelConfig::default());
    poll_without_delivering_network(
        &mut l,
        close_at + Duration::from_secs(31) + Duration::from_millis(1),
    )?;

    let replacement_stream_id = l
        .direct_api()
        .sctp_stream_id_by_channel_id(replacement)
        .expect("replacement should have a stream ID");
    assert_ne!(
        replacement_stream_id, old_stream_id,
        "elapsed time cannot make an unconfirmed reset safe; another free ID should be used"
    );

    Ok(())
}

/// Connects two peers built from `config` over the direct API and opens an out-of-band
/// negotiated channel on both, so no DCEP message occupies the send buffer.
fn connect_with_channel(
    config: impl Fn(RtcConfig) -> RtcConfig,
) -> Result<(TestRtc, TestRtc, ChannelId), RtcError> {
    let build = |peer: Peer| {
        let mut builder = Rtc::builder();
        if let Some(crypto) = peer.crypto_provider() {
            builder = builder.set_crypto_provider(crypto);
        }
        config(builder).build(Instant::now())
    };
    let (mut l, mut r) = connect_l_r_with_rtc(build(Peer::Left), build(Peer::Right));

    let channel = ChannelConfig {
        label: "data".into(),
        negotiated: Some(1),
        ..Default::default()
    };
    let cid = l.direct_api().create_data_channel(channel.clone());
    r.direct_api().create_data_channel(channel);

    loop {
        progress(&mut l, &mut r)?;
        let l_open = l
            .events
            .iter()
            .any(|(_, e)| matches!(e, Event::ChannelOpen(id, _) if *id == cid));
        let r_open = r
            .events
            .iter()
            .any(|(_, e)| matches!(e, Event::ChannelOpen(_, _)));
        if l_open && r_open {
            return Ok((l, r, cid));
        }
        assert!(
            l.duration() < Duration::from_secs(10),
            "channel should open on both peers"
        );
    }
}

/// Bytes of `ChannelData` the peer has received so far.
fn received_bytes(rtc: &TestRtc) -> usize {
    rtc.events
        .iter()
        .filter_map(|(_, e)| match e {
            Event::ChannelData(d) => Some(d.data.len()),
            _ => None,
        })
        .sum()
}

#[test]
fn data_channel_direct_configured_receive_limit() -> Result<(), RtcError> {
    init_crypto_default();

    const LIMIT: usize = 32 * 1024;
    let (mut l, mut r, cid) =
        connect_with_channel(|config| config.set_sctp_max_message_size(LIMIT as u32))?;

    let message = vec![1; LIMIT];
    assert!(l.channel(cid).unwrap().write(true, &message)?);
    assert!(r.channel(cid).unwrap().write(true, &message)?);
    while received_bytes(&l) < LIMIT || received_bytes(&r) < LIMIT {
        progress(&mut l, &mut r)?;
        assert!(l.duration() < Duration::from_secs(10));
    }

    // The direct API has no SDP, so send limits remain at the 64 KiB default.
    // Messages above our configured receive limit must still be dropped.
    let oversized = vec![2; LIMIT + 1];
    assert!(l.channel(cid).unwrap().write(true, &oversized)?);
    assert!(r.channel(cid).unwrap().write(true, &oversized)?);
    let start = l.duration();
    while l.duration() - start < Duration::from_secs(1) {
        progress(&mut l, &mut r)?;
    }
    for rtc in [&l, &r] {
        assert_eq!(received_bytes(rtc), LIMIT);
        assert!(
            rtc.events
                .iter()
                .any(|(_, e)| matches!(e, Event::ChannelData(data) if data.data == message))
        );
    }
    Ok(())
}

/// Keeps the send buffer full of 60 KiB messages for 3 seconds, then checks that all
/// of them arrive. This makes SCTP hand DTLS many packets at once.
fn keep_send_buffer_full(config: impl Fn(RtcConfig) -> RtcConfig) -> Result<(), RtcError> {
    let (mut l, mut r, cid) = connect_with_channel(config)?;

    let message = vec![0; 60 * 1024];
    let mut accepted = 0;
    let start = l.duration();
    while l.duration() - start < Duration::from_secs(3) {
        let mut chan = l.channel(cid).expect("open channel");
        while chan.write(true, &message)? {
            accepted += message.len();
        }
        progress(&mut l, &mut r)?;
    }

    while received_bytes(&r) < accepted {
        progress(&mut l, &mut r)?;
        assert!(
            l.duration() - start < Duration::from_secs(10),
            "accepted data should reach the remote peer"
        );
    }
    assert_eq!(received_bytes(&r), accepted);

    Ok(())
}

/// Handing DTLS the packets one stack frame per packet overflowed the stack of a debug
/// build.
#[test]
pub fn data_channel_full_send_buffer() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    keep_send_buffer_full(|config| config)
}

/// Writes 16 KiB messages without polling in between until `Channel::write` refuses
/// one. Nothing is sent or acknowledged meanwhile, so the accepted bytes are exactly
/// what the send buffer limit allows. Then checks that all of them arrive.
fn fill_send_buffer(max_buffered_amount: Option<usize>) -> Result<usize, RtcError> {
    const MESSAGE: usize = 16 * 1024;

    let (mut l, mut r, cid) = connect_with_channel(|config| match max_buffered_amount {
        Some(bytes) => config.set_sctp_max_buffered_amount(bytes),
        None => config,
    })?;

    let mut chan = l.channel(cid).expect("open channel");
    let mut accepted = 0;
    while accepted < 64 * MESSAGE && chan.write(true, &[0; MESSAGE])? {
        accepted += MESSAGE;
    }

    let start = l.duration();
    while received_bytes(&r) < accepted {
        progress(&mut l, &mut r)?;
        assert!(
            l.duration() - start < Duration::from_secs(10),
            "accepted data should reach the remote peer"
        );
    }

    Ok(accepted)
}

#[test]
pub fn data_channel_max_buffered_amount() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    // The default limit is unchanged.
    assert_eq!(fill_send_buffer(None)?, 128 * 1024);

    // A raised limit lets more than the default be buffered.
    assert_eq!(fill_send_buffer(Some(512 * 1024))?, 512 * 1024);

    Ok(())
}

#[test]
pub fn data_channel_full_large_send_buffer() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    keep_send_buffer_full(|config| config.set_sctp_max_buffered_amount(4 * 1024 * 1024))
}

#[test]
pub fn data_channel_flood() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let mut change = l.sdp_api();
    let cid = change.add_channel("My little channel".into());
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

    while l.channel(cid).is_none() {
        progress(&mut l, &mut r)?;
    }

    r.set_netem(NetemConfig::new().latency(Duration::from_millis(1000)));

    let mut count = 0;

    for _ in 0..10_000 {
        let mut chan = l.channel(cid).unwrap();
        let did_write = chan.write(true, &[0u8; 1400]).expect("to write string");
        if did_write {
            count += 1;
        }
        progress(&mut l, &mut r)?;
    }

    loop {
        progress(&mut l, &mut r)?;

        if l.duration() > Duration::from_secs(10) {
            break;
        }
    }
    assert!(count > 9000, "Too few events: {}", count);

    Ok(())
}

#[test]
pub fn channel_config_inband() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    // Create in-band negotiated channel (DCEP)
    let mut change = l.sdp_api();
    let cid = change.add_channel("DCEP Channel".into());
    let (offer, pending) = change.apply().unwrap();

    let answer = r.rtc.sdp_api().accept_offer(offer)?;
    l.rtc.sdp_api().accept_answer(pending, answer)?;

    // Wait for connection
    loop {
        if l.is_connected() && r.is_connected() {
            break;
        }
        progress(&mut l, &mut r)?;
    }

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let mut l_channel_opened = false;
    let mut r_channel_opened = false;
    let mut l_config_available_on_open = false;
    let mut r_config_available_on_open = false;

    // Process events and verify config availability immediately when ChannelOpen is fired
    loop {
        progress(&mut l, &mut r)?;

        // Check L side events and collect channel ID if found
        let mut l_found_id = None;
        for (_, event) in &l.events {
            if let Event::ChannelOpen(id, label) = event {
                if *id == cid && label == "DCEP Channel" {
                    l_channel_opened = true;
                    l_found_id = Some(*id);
                    break;
                }
            }
        }

        // Check R side events and collect channel ID if found
        let mut r_found_id = None;
        for (_, event) in &r.events {
            if let Event::ChannelOpen(id, label) = event {
                if label == "DCEP Channel" {
                    r_channel_opened = true;
                    r_found_id = Some(*id);
                    break;
                }
            }
        }

        // Verify config is available immediately when ChannelOpen is emitted
        if let Some(id) = l_found_id {
            if let Some(channel) = l.channel(id) {
                l_config_available_on_open = channel.config().is_some();
            }
        }

        if let Some(id) = r_found_id {
            if let Some(channel) = r.channel(id) {
                r_config_available_on_open = channel.config().is_some();
            }
        }

        if (l_channel_opened && r_channel_opened) || l.duration() > Duration::from_secs(10) {
            break;
        }
    }

    assert!(l_channel_opened, "L side should receive ChannelOpen event");
    assert!(r_channel_opened, "R side should receive ChannelOpen event");
    assert!(
        l_config_available_on_open,
        "L side config should be available on ChannelOpen"
    );
    assert!(
        r_config_available_on_open,
        "R side config should be available on ChannelOpen"
    );

    Ok(())
}

#[test]
pub fn channel_config_outband_local() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    // Enable SCTP by adding a temporary channel (will be removed)
    let mut change_l = l.sdp_api();
    let _temp_cid = change_l.add_channel("temp".into());
    let (offer, pending) = change_l.apply().unwrap();

    let answer = r.rtc.sdp_api().accept_offer(offer)?;
    l.rtc.sdp_api().accept_answer(pending, answer)?;

    // Wait for connection
    loop {
        if l.is_connected() && r.is_connected() {
            break;
        }
        progress(&mut l, &mut r)?;
    }

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    // Wait for SCTP to be established first
    loop {
        progress(&mut l, &mut r)?;

        // Check for SCTP connection via any channel events
        let connected = l
            .events
            .iter()
            .any(|(_, e)| matches!(e, Event::ChannelOpen(_, _)))
            || r.events
                .iter()
                .any(|(_, e)| matches!(e, Event::ChannelOpen(_, _)));

        if connected || l.duration() > Duration::from_secs(5) {
            break;
        }
    }

    // Create out-of-band negotiated channel on both sides
    let config = ChannelConfig {
        negotiated: Some(10),
        label: "OutOfBand Local".into(),
        ..Default::default()
    };

    let cid_l = l.direct_api().create_data_channel(config.clone());
    let cid_r = r.direct_api().create_data_channel(config);

    // Allow some time for channels to be established
    for _ in 0..10 {
        progress(&mut l, &mut r)?;
    }

    // Verify config is immediately available for locally created out-of-band channels
    let l_channel = l.channel(cid_l).expect("L channel should be available");
    let r_channel = r.channel(cid_r).expect("R channel should be available");

    assert!(
        l_channel.config().is_some(),
        "L side config should be immediately available for local out-of-band channel"
    );
    assert!(
        r_channel.config().is_some(),
        "R side config should be immediately available for local out-of-band channel"
    );

    let l_config = l_channel.config().unwrap();
    let r_config = r_channel.config().unwrap();

    assert_eq!(l_config.label, "OutOfBand Local");
    assert_eq!(r_config.label, "OutOfBand Local");
    assert_eq!(l_config.negotiated, Some(10));
    assert_eq!(r_config.negotiated, Some(10));

    Ok(())
}

#[test]
pub fn channel_config_with_protocol() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let mut change = l.sdp_api();
    let _temp_cid = change.add_channel("temp".into());
    let (offer, pending) = change.apply().unwrap();

    let answer = r.rtc.sdp_api().accept_offer(offer)?;
    l.rtc.sdp_api().accept_answer(pending, answer)?;

    // Wait for connection
    loop {
        if l.is_connected() && r.is_connected() {
            break;
        }
        progress(&mut l, &mut r)?;
    }

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    // Wait for SCTP to be established
    loop {
        progress(&mut l, &mut r)?;
        let connected = l
            .events
            .iter()
            .any(|(_, e)| matches!(e, Event::ChannelOpen(_, _)));
        if connected || l.duration() > Duration::from_secs(5) {
            break;
        }
    }

    // Create channels with custom protocol
    let custom_protocol = "my-custom-protocol";
    let config = ChannelConfig {
        negotiated: Some(20),
        protocol: custom_protocol.into(),
        ..Default::default()
    };

    let cid_l = l.direct_api().create_data_channel(config.clone());
    let cid_r = r.direct_api().create_data_channel(config);

    for _ in 0..10 {
        progress(&mut l, &mut r)?;
    }

    // Verify protocol is correctly set on both sides
    let l_channel = l.channel(cid_l).unwrap();
    let r_channel = r.channel(cid_r).unwrap();
    let l_config = l_channel.config().unwrap();
    let r_config = r_channel.config().unwrap();

    assert_eq!(l_config.protocol, custom_protocol);
    assert_eq!(r_config.protocol, custom_protocol);

    Ok(())
}
