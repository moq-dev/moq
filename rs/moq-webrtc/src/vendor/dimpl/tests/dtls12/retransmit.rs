//! DTLS 1.2 retransmission and duplicate handling tests.

#![allow(unused)]

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dimpl::{Config, Dtls, Output};

use crate::common::*;

#[cfg(feature = "rcgen")]
struct FinalFlightResend {
    client: Dtls,
    server: Dtls,
    f6_init: Vec<Vec<u8>>,
    f6_resend: Vec<Vec<u8>>,
    stale_epoch0_handshake: Vec<u8>,
}

#[cfg(feature = "rcgen")]
fn prepare_server_final_flight_resend(max_queue_rx: usize) -> FinalFlightResend {
    use dimpl::certificate::generate_self_signed_certificate;

    let mut now = Instant::now();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config_client = Arc::new(
        Config::builder()
            .mtu(115)
            .max_queue_rx(max_queue_rx)
            .build()
            .expect("Failed to build config"),
    );
    let config_server = Arc::new(
        Config::builder()
            .mtu(115)
            .max_queue_rx(max_queue_rx)
            .build()
            .expect("Failed to build config"),
    );

    let mut client = Dtls::new_12(config_client, client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_12(config_server, server_cert, now);
    server.set_active(false);

    client.handle_timeout(now).expect("client timeout start");
    client.handle_timeout(now).expect("client arm flight 1");
    let f1 = collect_packets(&mut client);
    for packet in f1 {
        server.handle_packet(&packet).expect("server recv f1");
    }

    server.handle_timeout(now).expect("server arm flight 2");
    let f2 = collect_packets(&mut server);
    assert!(!f2.is_empty(), "server should emit flight 2 after CH");
    for packet in f2 {
        client.handle_packet(&packet).expect("client recv f2");
    }

    client.handle_timeout(now).expect("client arm flight 3");
    let f3 = collect_packets(&mut client);
    assert!(!f3.is_empty(), "client should emit flight 3 after HVR");
    let stale_epoch0_handshake = first_record_matching(&f3, 22, 0)
        .expect("flight 3 should contain a plaintext ClientHello record");
    for packet in f3 {
        server.handle_packet(&packet).expect("server recv f3");
    }

    server.handle_timeout(now).expect("server arm flight 4");
    let f4 = collect_packets(&mut server);
    assert!(
        !f4.is_empty(),
        "server should emit flight 4 after CH+cookie"
    );
    for packet in f4 {
        client.handle_packet(&packet).expect("client recv f4");
    }

    client.handle_timeout(now).expect("client arm flight 5");
    let f5_init = collect_packets(&mut client);
    assert!(
        !f5_init.is_empty(),
        "client should emit flight 5 after server flight"
    );
    for packet in &f5_init {
        server.handle_packet(packet).expect("server recv f5");
    }

    server.handle_timeout(now).expect("server arm flight 6");
    let f6_init = collect_packets(&mut server);
    assert!(!f6_init.is_empty(), "server should emit initial flight 6");
    let f6_init_hdrs = collect_headers(&f6_init);
    assert!(
        f6_init_hdrs.iter().any(|h| h.ctype == 20 && h.epoch == 0),
        "server flight 6 should include epoch 0 CCS"
    );
    assert!(
        f6_init_hdrs.iter().any(|h| h.ctype == 22 && h.epoch == 1),
        "server flight 6 should include epoch 1 Finished"
    );

    trigger_timeout(&mut client, &mut now);
    let f5_resend = collect_packets(&mut client);
    assert!(!f5_resend.is_empty(), "client should resend flight 5");
    for packet in &f5_resend {
        server.handle_packet(packet).expect("server recv f5 resend");
    }

    let f6_resend = collect_packets(&mut server);
    assert!(
        !f6_resend.is_empty(),
        "server should resend flight 6 upon receiving duplicate Finished"
    );
    let f6_resend_hdrs = collect_headers(&f6_resend);
    assert!(
        f6_resend_hdrs.iter().any(|h| h.ctype == 22 && h.epoch == 1),
        "resend flight 6 should include epoch 1 Finished"
    );
    assert_epochs_and_seq_increased(&f6_init_hdrs, &f6_resend_hdrs);
    server
        .handle_timeout(now)
        .expect("arm server resend cooldown");
    assert!(collect_packets(&mut server).is_empty());

    FinalFlightResend {
        client,
        server,
        f6_init,
        f6_resend,
        stale_epoch0_handshake,
    }
}

#[cfg(feature = "rcgen")]
fn first_record_matching(datagrams: &[Vec<u8>], content_type: u8, epoch: u16) -> Option<Vec<u8>> {
    for datagram in datagrams {
        let mut offset = 0usize;
        while offset + 13 <= datagram.len() {
            let len = u16::from_be_bytes([datagram[offset + 11], datagram[offset + 12]]) as usize;
            let end = offset + 13 + len;
            if end > datagram.len() {
                break;
            }

            let record_epoch = u16::from_be_bytes([datagram[offset + 3], datagram[offset + 4]]);
            if datagram[offset] == content_type && record_epoch == epoch {
                return Some(datagram[offset..end].to_vec());
            }

            offset = end;
        }
    }

    None
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_client_hello_retransmits_using_advertised_deadlines() {
    use dimpl::certificate::generate_self_signed_certificate;

    let now = Instant::now();
    let certificate = generate_self_signed_certificate().expect("generate client certificate");
    let mut client = Dtls::new_12(Arc::new(Config::default()), certificate, now);
    client.set_active(true);

    client.handle_timeout(now).expect("start client handshake");
    let initial_output = drain_outputs(&mut client);
    assert!(
        !initial_output.packets.is_empty(),
        "client should emit ClientHello"
    );

    let retransmit_at = initial_output
        .timeout
        .expect("client should advertise a retransmission deadline");
    assert!(retransmit_at >= now + Duration::from_millis(750));
    assert!(retransmit_at <= now + Duration::from_millis(1250));
    client
        .handle_timeout(retransmit_at - Duration::from_nanos(1))
        .expect("before ClientHello retransmission");
    let before_retransmission = drain_outputs(&mut client);
    assert!(
        before_retransmission.packets.is_empty(),
        "must not retransmit early"
    );
    assert_eq!(before_retransmission.timeout, Some(retransmit_at));
    client
        .handle_timeout(retransmit_at)
        .expect("retransmit ClientHello");
    let retransmission_output = drain_outputs(&mut client);

    assert!(
        !retransmission_output.packets.is_empty(),
        "the dropped ClientHello flight was not retransmitted"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_resends_each_flight_epoch_and_sequence_increase() {
    let now0 = Instant::now();
    let mut now = now0;

    use dimpl::certificate::generate_self_signed_certificate;

    // Certificates for client and server
    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config_client = Arc::new(Config::default());
    let config_server = Arc::new(Config::default());

    // Client
    let mut client = Dtls::new_12(config_client, client_cert.clone(), now);
    client.set_active(true);

    // Server
    let mut server = Dtls::new_12(config_server, server_cert.clone(), now);
    server.set_active(false);

    // FLIGHT 1 (ClientHello): block initial, deliver resend
    client.handle_timeout(now).expect("client timeout start");
    // flight_begin reset the flight timer; arm it again so poll_output yields packets
    client.handle_timeout(now).expect("client arm flight 1");
    let init1_pkts = collect_packets(&mut client);
    let init1_hdrs = collect_headers(&init1_pkts);
    trigger_timeout(&mut client, &mut now);
    let resend1_pkts = collect_packets(&mut client);
    let resend1_hdrs = collect_headers(&resend1_pkts);
    assert_epochs_and_seq_increased(&init1_hdrs, &resend1_hdrs);
    for p in resend1_pkts {
        server.handle_packet(&p).expect("server recv f1");
    }

    // FLIGHT 2 (HelloVerifyRequest): capture initial from server, block, deliver resend
    server.handle_timeout(now).expect("server arm flight 2");
    let init2_pkts = collect_packets(&mut server);
    assert!(
        !init2_pkts.is_empty(),
        "server should emit flight 2 after CH"
    );
    let init2_hdrs = collect_headers(&init2_pkts);
    trigger_timeout(&mut server, &mut now);
    let resend2_pkts = collect_packets(&mut server);
    let resend2_hdrs = collect_headers(&resend2_pkts);
    assert_epochs_and_seq_increased(&init2_hdrs, &resend2_hdrs);
    for p in resend2_pkts {
        client.handle_packet(&p).expect("client recv f2");
    }

    // FLIGHT 3 (ClientHello with cookie): block initial, deliver resend
    client.handle_timeout(now).expect("client arm flight 3");
    let init3_pkts = collect_packets(&mut client);
    assert!(
        !init3_pkts.is_empty(),
        "client should emit flight 3 after HVR"
    );
    let init3_hdrs = collect_headers(&init3_pkts);
    trigger_timeout(&mut client, &mut now);
    let resend3_pkts = collect_packets(&mut client);
    let resend3_hdrs = collect_headers(&resend3_pkts);
    assert_epochs_and_seq_increased(&init3_hdrs, &resend3_hdrs);
    for p in resend3_pkts {
        server.handle_packet(&p).expect("server recv f3");
    }

    // FLIGHT 4 (ServerHello, Certificate, SKE, CR, SHD): block initial, deliver resend
    server.handle_timeout(now).expect("server arm flight 4");
    let init4_pkts = collect_packets(&mut server);
    assert!(
        !init4_pkts.is_empty(),
        "server should emit flight 4 after CH+cookie"
    );
    let init4_hdrs = collect_headers(&init4_pkts);
    trigger_timeout(&mut server, &mut now);
    let resend4_pkts = collect_packets(&mut server);
    let resend4_hdrs = collect_headers(&resend4_pkts);
    assert_epochs_and_seq_increased(&init4_hdrs, &resend4_hdrs);
    for p in resend4_pkts {
        client.handle_packet(&p).expect("client recv f4");
    }

    // FLIGHT 5 (Client cert?, CKX, CV?, CCS, Finished): block initial, deliver resend
    client.handle_timeout(now).expect("client arm flight 5");
    let init5_pkts = collect_packets(&mut client);
    assert!(
        !init5_pkts.is_empty(),
        "client should emit flight 5 after server flight"
    );
    let init5_hdrs = collect_headers(&init5_pkts);
    trigger_timeout(&mut client, &mut now);
    let resend5_pkts = collect_packets(&mut client);
    let resend5_hdrs = collect_headers(&resend5_pkts);
    assert_epochs_and_seq_increased(&init5_hdrs, &resend5_hdrs);
    // Additionally, ensure Finished is epoch 1 is present in the set
    assert!(
        resend5_hdrs.iter().any(|h| h.ctype == 22 && h.epoch == 1),
        "client flight 5 should include epoch 1 Finished"
    );
    for p in resend5_pkts {
        server.handle_packet(&p).expect("server recv f5");
    }

    // FLIGHT 6 (Server CCS, Finished): no resend timer after final flight
    server.handle_timeout(now).expect("server arm flight 6");
    let init6_pkts = collect_packets(&mut server);
    assert!(
        !init6_pkts.is_empty(),
        "server should emit flight 6 after client flight 5"
    );
    let init6_hdrs = collect_headers(&init6_pkts);
    // Final flight should include epoch 1 Finished in the initial transmission
    assert!(
        init6_hdrs.iter().any(|h| h.ctype == 22 && h.epoch == 1),
        "server flight 6 should include epoch 1 Finished"
    );
    // Ensure no timer-driven resend occurs after final flight
    trigger_timeout(&mut server, &mut now);
    let resend6_pkts = collect_packets(&mut server);
    assert!(resend6_pkts.is_empty(), "no resend after final flight");
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_duplicate_triggers_server_resend_of_final_flight() {
    let result = prepare_server_final_flight_resend(Config::default().max_queue_rx());
    assert!(
        !result.f6_init.is_empty(),
        "server should emit initial flight 6"
    );
    assert!(
        !result.f6_resend.is_empty(),
        "server should resend flight 6 upon receiving duplicate Finished"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_resend_replaces_pending_handshake_flight_tail() {
    let now0 = Instant::now();
    let mut now = now0;

    use dimpl::certificate::generate_self_signed_certificate;

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config_client = Arc::new(
        Config::builder()
            .mtu(115)
            .build()
            .expect("Failed to build config"),
    );
    let config_server = Arc::new(
        Config::builder()
            .mtu(115)
            .build()
            .expect("Failed to build config"),
    );

    let expected_initial_headers = {
        let mut client = Dtls::new_12(Arc::clone(&config_client), client_cert.clone(), now0);
        client.set_active(true);

        let mut server = Dtls::new_12(Arc::clone(&config_server), server_cert.clone(), now0);
        server.set_active(false);

        client.handle_timeout(now0).expect("client timeout start");
        client.handle_timeout(now0).expect("client arm flight 1");
        for p in collect_packets(&mut client) {
            server.handle_packet(&p).expect("control server recv f1");
        }

        server
            .handle_timeout(now0)
            .expect("control server arm flight 2");
        for p in collect_packets(&mut server) {
            client.handle_packet(&p).expect("control client recv f2");
        }

        client
            .handle_timeout(now0)
            .expect("control client arm flight 3");
        for p in collect_packets(&mut client) {
            server.handle_packet(&p).expect("control server recv f3");
        }

        server
            .handle_timeout(now0)
            .expect("control server arm flight 4");
        collect_headers(&collect_packets(&mut server))
    };
    assert!(
        !expected_initial_headers.is_empty(),
        "control flight 4 should contain records"
    );

    let mut client = Dtls::new_12(config_client, client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_12(config_server, server_cert, now);
    server.set_active(false);

    client.handle_timeout(now).expect("client timeout start");
    client.handle_timeout(now).expect("client arm flight 1");
    let f1 = collect_packets(&mut client);
    for p in f1 {
        server.handle_packet(&p).expect("server recv f1");
    }

    server.handle_timeout(now).expect("server arm flight 2");
    let f2 = collect_packets(&mut server);
    for p in f2 {
        client.handle_packet(&p).expect("client recv f2");
    }

    client.handle_timeout(now).expect("client arm flight 3");
    let f3 = collect_packets(&mut client);
    for p in f3 {
        server.handle_packet(&p).expect("server recv f3");
    }

    server.handle_timeout(now).expect("server arm flight 4");
    let first_initial = poll_one_packet(&mut server);
    let first_initial_headers = parse_records(&first_initial);
    assert!(
        !first_initial_headers.is_empty(),
        "first initial flight 4 packet should contain records"
    );

    trigger_timeout(&mut server, &mut now);
    let pending = collect_packets(&mut server);
    assert!(
        !pending.is_empty(),
        "server should emit flight 4 resend after timeout"
    );
    let pending_headers = collect_headers(&pending);
    assert_eq!(
        pending_headers.len(),
        expected_initial_headers.len(),
        "resend should replace the undrained original flight tail instead of appending to it; \
         expected initial {expected_initial_headers:?}, pending {pending_headers:?}"
    );
    assert!(
        pending_headers
            .iter()
            .zip(expected_initial_headers.iter())
            .all(|(resend, initial)| resend.ctype == initial.ctype
                && resend.epoch == initial.epoch
                && resend.seq > initial.seq),
        "resend should contain only a fresh copy of the flight, got expected initial \
         {expected_initial_headers:?}, first polled {first_initial_headers:?}, pending \
         {pending_headers:?}"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_final_flight_resend_can_share_post_release_appdata_tail() {
    let FinalFlightResend {
        mut server,
        stale_epoch0_handshake,
        ..
    } = prepare_server_final_flight_resend(Config::default().max_queue_rx());

    let cooldown = drain_outputs(&mut server).timeout.expect("resend cooldown");
    server
        .handle_timeout(cooldown)
        .expect("expire previous resend cooldown");
    assert!(collect_packets(&mut server).is_empty());

    let filler = vec![0x55; 50];
    for i in 0..9 {
        server
            .send_application_data(&filler)
            .unwrap_or_else(|err| panic!("queue filler app data {i}: {err:?}"));
    }
    server
        .send_application_data(b"x")
        .expect("queue final small app data");

    server
        .handle_packet(&stale_epoch0_handshake)
        .expect("post-release duplicate should trigger final-flight resend");

    let pending_headers = collect_headers(&collect_packets(&mut server));
    let app_data_records = pending_headers
        .iter()
        .filter(|header| header.ctype == 23 && header.epoch == 1)
        .count();
    assert_eq!(
        app_data_records, 10,
        "post-release final-flight resend must preserve queued app-data records, got \
         {pending_headers:?}"
    );
    assert!(
        pending_headers
            .iter()
            .any(|header| header.ctype == 22 && header.epoch == 1),
        "server should retain and resend encrypted final-flight Finished, got \
         {pending_headers:?}"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_late_retransmitted_ccs_does_not_pin_receive_queue() {
    //! After the handshake, a peer can still retransmit its final flight. The
    //! epoch-0 ChangeCipherSpec in that flight must be ignored; otherwise it
    //! remains unhandled and prevents handled app-data records behind it from
    //! being purged.

    const RX_QUEUE_LIMIT: usize = 8;

    let FinalFlightResend {
        mut client,
        mut server,
        f6_init,
        f6_resend,
        stale_epoch0_handshake: _,
    } = prepare_server_final_flight_resend(RX_QUEUE_LIMIT);

    deliver_packets(&f6_init, &mut client);
    let client_connected = drain_outputs(&mut client).connected;
    assert!(
        client_connected,
        "client should connect after initial flight 6"
    );

    for packet in &f6_resend {
        client
            .handle_packet(packet)
            .expect("late final-flight resend should be tolerated");
    }

    for i in 0..=RX_QUEUE_LIMIT {
        let msg = format!("post-ccs app data {i}");
        server
            .send_application_data(msg.as_bytes())
            .expect("send app data");

        let server_out = drain_outputs(&mut server);
        assert!(
            !server_out.packets.is_empty(),
            "server should emit app-data packets"
        );

        for packet in &server_out.packets {
            client
                .handle_packet(packet)
                .expect("late CCS must not make receive queue fill");
        }

        let client_out = drain_outputs(&mut client);
        assert!(
            client_out
                .app_data
                .iter()
                .any(|received| received == msg.as_bytes()),
            "client should receive app data after late CCS"
        );
    }
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_late_epoch0_handshake_does_not_pin_receive_queue() {
    //! After peer encryption is enabled, plaintext epoch-0 handshake records are
    //! unauthenticated and no longer actionable. They must be ignored for the
    //! same queue-pinning reason as late CCS records.

    const RX_QUEUE_LIMIT: usize = 8;

    let FinalFlightResend {
        mut client,
        mut server,
        f6_init,
        f6_resend: _,
        stale_epoch0_handshake: _,
    } = prepare_server_final_flight_resend(RX_QUEUE_LIMIT);

    deliver_packets(&f6_init, &mut client);
    let client_connected = drain_outputs(&mut client).connected;
    assert!(
        client_connected,
        "client should connect after initial flight 6"
    );

    client
        .handle_packet(&malformed_epoch0_handshake_packet(0x7fff))
        .expect("late epoch-0 handshake should be tolerated");

    for i in 0..=RX_QUEUE_LIMIT {
        let msg = format!("post-handshake app data {i}");
        server
            .send_application_data(msg.as_bytes())
            .expect("send app data");

        let server_out = drain_outputs(&mut server);
        assert!(
            !server_out.packets.is_empty(),
            "server should emit app-data packets"
        );

        for packet in &server_out.packets {
            client
                .handle_packet(packet)
                .expect("late epoch-0 handshake must not make receive queue fill");
        }

        let client_out = drain_outputs(&mut client);
        assert!(
            client_out
                .app_data
                .iter()
                .any(|received| received == msg.as_bytes()),
            "client should receive app data after late epoch-0 handshake"
        );
    }
}

fn malformed_epoch0_handshake_packet(sequence_number: u64) -> Vec<u8> {
    let sequence_bytes = sequence_number.to_be_bytes();
    let mut packet = Vec::with_capacity(14);
    packet.push(22);
    packet.extend_from_slice(&[0xfe, 0xfd]);
    packet.extend_from_slice(&0u16.to_be_bytes());
    packet.extend_from_slice(&sequence_bytes[2..]);
    packet.extend_from_slice(&1u16.to_be_bytes());
    packet.push(0xff);
    packet
}

fn append_record(mut packet: Vec<u8>, record: &[u8]) -> Vec<u8> {
    packet.extend_from_slice(record);
    packet
}

#[cfg(feature = "rcgen")]
fn poll_one_packet(endpoint: &mut Dtls) -> Vec<u8> {
    let mut buf = vec![0u8; 2048];
    loop {
        match endpoint.poll_output(&mut buf) {
            Output::Packet(packet) => return packet.to_vec(),
            Output::Timeout(_) => panic!("expected queued packet"),
            _ => {}
        }
    }
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_late_epoch0_handshake_trailing_app_data_does_not_pin_receive_queue() {
    //! A stale epoch-0 handshake can arrive behind a valid encrypted record in
    //! the same datagram. The encrypted app data should still be delivered, and
    //! the stale plaintext handshake must not keep that datagram in the queue.

    const RX_QUEUE_LIMIT: usize = 8;

    let FinalFlightResend {
        mut client,
        mut server,
        f6_init,
        f6_resend: _,
        stale_epoch0_handshake,
    } = prepare_server_final_flight_resend(RX_QUEUE_LIMIT);

    deliver_packets(&f6_init, &mut client);
    let client_connected = drain_outputs(&mut client).connected;
    assert!(
        client_connected,
        "client should connect after initial flight 6"
    );

    for i in 0..=RX_QUEUE_LIMIT {
        let msg = format!("post-handshake app data {i}");
        server
            .send_application_data(msg.as_bytes())
            .expect("send app data");

        let server_out = drain_outputs(&mut server);
        assert!(
            !server_out.packets.is_empty(),
            "server should emit app-data packets"
        );

        for packet in server_out.packets {
            let packet = if i == 0 {
                append_record(packet, &stale_epoch0_handshake)
            } else {
                packet
            };
            client
                .handle_packet(&packet)
                .expect("trailing late epoch-0 handshake must not make receive queue fill");
        }

        let client_out = drain_outputs(&mut client);
        assert!(
            client_out
                .app_data
                .iter()
                .any(|received| received == msg.as_bytes()),
            "client should receive app data after trailing late epoch-0 handshake"
        );
    }
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_handshake_completes_after_packet_loss() {
    use dimpl::certificate::generate_self_signed_certificate;

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls12_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_12(config, server_cert, now);
    server.set_active(false);

    let mut client_connected = false;
    let mut server_connected = false;
    let mut drop_next_client_packet = true; // Drop first ClientHello

    for i in 0..60 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        if client_out.connected {
            client_connected = true;
        }
        if server_out.connected {
            server_connected = true;
        }

        // Simulate packet loss: drop first client packet batch
        if !client_out.packets.is_empty() && drop_next_client_packet {
            drop_next_client_packet = false;
            // Don't deliver client packets this round
        } else {
            deliver_packets(&client_out.packets, &mut server);
        }

        deliver_packets(&server_out.packets, &mut client);

        if client_connected && server_connected {
            break;
        }

        // Advance time to trigger retransmissions periodically
        if i % 5 == 4 {
            now += Duration::from_secs(2);
        } else {
            now += Duration::from_millis(10);
        }
    }

    assert!(
        client_connected,
        "Client should connect despite initial packet loss"
    );
    assert!(
        server_connected,
        "Server should connect despite initial packet loss"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_handshake_completes_with_early_packet_loss() {
    use dimpl::certificate::generate_self_signed_certificate;

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    // Use a config with more retries to handle packet loss
    let config = Arc::new(
        Config::builder()
            .flight_retries(8)
            .build()
            .expect("Failed to build config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_12(config, server_cert, now);
    server.set_active(false);

    let mut client_connected = false;
    let mut server_connected = false;

    // Drop first 2 client packets and first 2 server packets to test retransmission
    let mut client_packets_to_drop = 2;
    let mut server_packets_to_drop = 2;

    for i in 0..60 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        if client_out.connected {
            client_connected = true;
        }
        if server_out.connected {
            server_connected = true;
        }

        // Deliver client packets, dropping first N
        for packet in &client_out.packets {
            if client_packets_to_drop > 0 {
                client_packets_to_drop -= 1;
            } else {
                let _ = server.handle_packet(packet);
            }
        }

        // Deliver server packets, dropping first N
        for packet in &server_out.packets {
            if server_packets_to_drop > 0 {
                server_packets_to_drop -= 1;
            } else {
                let _ = client.handle_packet(packet);
            }
        }

        if client_connected && server_connected {
            break;
        }

        // Trigger retransmissions periodically
        if i % 5 == 4 {
            now += Duration::from_secs(2);
        } else {
            now += Duration::from_millis(10);
        }
    }

    assert!(
        client_connected,
        "Client should connect despite early packet loss"
    );
    assert!(
        server_connected,
        "Server should connect despite early packet loss"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_survives_random_packet_loss_pattern() {
    use dimpl::certificate::generate_self_signed_certificate;

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = Arc::new(
        Config::builder()
            .flight_retries(8)
            .build()
            .expect("Failed to build config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_12(config, server_cert, now);
    server.set_active(false);

    let mut client_connected = false;
    let mut server_connected = false;
    let mut total_dropped = 0;
    let mut total_delivered = 0;

    // Deterministic pattern: drop every 3rd packet across both directions
    let mut global_packet_index = 0usize;

    for i in 0..100 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        if client_out.connected {
            client_connected = true;
        }
        if server_out.connected {
            server_connected = true;
        }

        for packet in &client_out.packets {
            if global_packet_index % 3 == 2 {
                total_dropped += 1;
            } else {
                let _ = server.handle_packet(packet);
                total_delivered += 1;
            }
            global_packet_index += 1;
        }

        for packet in &server_out.packets {
            if global_packet_index % 3 == 2 {
                total_dropped += 1;
            } else {
                let _ = client.handle_packet(packet);
                total_delivered += 1;
            }
            global_packet_index += 1;
        }

        if client_connected && server_connected {
            break;
        }

        // Trigger retransmissions periodically
        if i % 5 == 4 {
            now += Duration::from_secs(2);
        } else {
            now += Duration::from_millis(10);
        }
    }

    assert!(
        client_connected,
        "Client should connect despite every-3rd-packet loss"
    );
    assert!(
        server_connected,
        "Server should connect despite every-3rd-packet loss"
    );
    assert!(
        total_dropped > 0,
        "Test should have dropped at least one packet"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_retransmit_exponential_backoff() {
    use dimpl::certificate::generate_self_signed_certificate;

    let client_cert = generate_self_signed_certificate().expect("gen client cert");

    // Use a known start RTO and enough retries to observe backoff
    let config = Arc::new(
        Config::builder()
            .flight_start_rto(Duration::from_secs(1))
            .flight_retries(4)
            .handshake_timeout(Duration::from_secs(120))
            .build()
            .expect("Failed to build config"),
    );

    let mut now = Instant::now();
    let mut client = Dtls::new_12(config, client_cert, now);
    client.set_active(true);

    // No server -- we never deliver packets.
    // Start the handshake.
    client.handle_timeout(now).expect("client timeout start");
    client.handle_timeout(now).expect("client arm flight 1");
    let _ = collect_packets(&mut client);

    // Record successive timeout values returned by poll_output.
    // Each handle_timeout that fires the flight timer should produce a new,
    // larger timeout.
    let mut timeouts: Vec<Instant> = Vec::new();

    // Collect the first timeout
    let mut buf = vec![0u8; 2048];
    loop {
        if let Output::Timeout(t) = client.poll_output(&mut buf) {
            timeouts.push(t);
            break;
        }
    }

    // Fire successive timeouts, never delivering packets
    for _ in 0..3 {
        // Advance past the reported timeout
        now = *timeouts.last().expect("should have a timeout") + Duration::from_millis(1);
        client.handle_timeout(now).expect("client handle_timeout");
        let _ = collect_packets(&mut client);

        // Collect the next timeout
        loop {
            if let Output::Timeout(t) = client.poll_output(&mut buf) {
                timeouts.push(t);
                break;
            }
        }
    }

    // Verify we collected multiple timeout values
    assert!(
        timeouts.len() >= 4,
        "Should have at least 4 timeout values, got {}",
        timeouts.len()
    );

    // Verify each successive interval is strictly larger (exponential backoff).
    // Interval[i] = timeout[i+1] - timeout[i] should be increasing.
    let mut intervals: Vec<Duration> = Vec::new();
    for pair in timeouts.windows(2) {
        let interval = pair[1].duration_since(pair[0]);
        intervals.push(interval);
    }

    for pair in intervals.windows(2) {
        assert!(
            pair[1] > pair[0],
            "Backoff intervals should increase: {:?} should be > {:?}",
            pair[1],
            pair[0]
        );
    }
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_handshake_timeout_aborts() {
    use dimpl::certificate::generate_self_signed_certificate;

    let client_cert = generate_self_signed_certificate().expect("gen client cert");

    // Configure a very short handshake timeout so we hit it quickly
    let config = Arc::new(
        Config::builder()
            .handshake_timeout(Duration::from_secs(5))
            .flight_start_rto(Duration::from_secs(1))
            .flight_retries(10) // Many retries, but the handshake timeout will fire first
            .build()
            .expect("Failed to build config"),
    );

    let mut now = Instant::now();
    let mut client = Dtls::new_12(config, client_cert, now);
    client.set_active(true);

    // Start the handshake
    client.handle_timeout(now).expect("client timeout start");
    client.handle_timeout(now).expect("client arm flight 1");
    let _ = collect_packets(&mut client);

    // Keep triggering timeouts without ever delivering packets.
    // The handshake_timeout (5s) should eventually cause handle_timeout to return an error.
    let mut got_timeout_error = false;
    for _ in 0..100 {
        now += Duration::from_secs(1);
        match client.handle_timeout(now) {
            Ok(()) => {
                // Drain any packets to keep the state machine consistent
                let _ = collect_packets(&mut client);
            }
            Err(e) => {
                let msg = format!("{}", e);
                assert!(
                    msg.contains("timeout"),
                    "Expected timeout error, got: {}",
                    msg
                );
                got_timeout_error = true;
                break;
            }
        }
    }

    assert!(
        got_timeout_error,
        "Client should report a timeout error when handshake_timeout is exceeded"
    );
}

/// Before the peer is confirmed (the legitimate retransmission window), a stale
/// plaintext ClientHello still triggers a flight-6 resend, so a lost flight 6
/// recovers. This is the contrast case to the post-confirmation suppression
/// below: the gate must not break legitimate retransmission.
#[test]
#[cfg(feature = "rcgen")]
fn dtls12_stale_client_hello_before_peer_confirmed_triggers_resend() {
    let _ = env_logger::try_init();

    const RX_QUEUE_LIMIT: usize = 8;
    let FinalFlightResend {
        mut server,
        stale_epoch0_handshake,
        ..
    } = prepare_server_final_flight_resend(RX_QUEUE_LIMIT);

    let cooldown = drain_outputs(&mut server).timeout.expect("resend cooldown");
    server
        .handle_timeout(cooldown)
        .expect("expire previous resend cooldown");
    assert!(collect_packets(&mut server).is_empty());

    // The server has sent flight 6 but has no confirmation the client received
    // it (no authenticated application data has arrived). A stale ClientHello
    // here means "peer still needs my flight" and must drive a resend.
    server
        .handle_packet(&stale_epoch0_handshake)
        .expect("stale ClientHello must be tolerated");
    let resend = collect_packets(&mut server);

    assert!(
        !resend.is_empty(),
        "server must resend flight 6 on a stale ClientHello while the peer is unconfirmed"
    );
}

/// Once the peer is confirmed to have completed the handshake (the server has
/// received authenticated application data from the client), a replayed
/// plaintext ClientHello is unauthenticated noise and must not trigger a flight
/// retransmission. This closes the resend-amplification window that an attacker
/// could otherwise drive with spoofed epoch-0 handshakes.
#[test]
#[cfg(feature = "rcgen")]
fn dtls12_stale_client_hello_after_peer_confirmed_triggers_no_resend() {
    let _ = env_logger::try_init();

    const RX_QUEUE_LIMIT: usize = 8;
    let FinalFlightResend {
        mut client,
        mut server,
        f6_resend,
        stale_epoch0_handshake,
        ..
    } = prepare_server_final_flight_resend(RX_QUEUE_LIMIT);

    // Finish the handshake: deliver the server's resent flight 6 to the client.
    deliver_packets(&f6_resend, &mut client);
    assert!(
        drain_outputs(&mut client).connected,
        "client should connect once it receives flight 6"
    );

    // Client application data is the server's signal that the client completed
    // (it could only send it after receiving flight 6).
    client
        .send_application_data(b"client-app-data")
        .expect("client sends application data");
    let client_app = collect_packets(&mut client);
    assert!(!client_app.is_empty(), "client should emit app-data packet");
    deliver_packets(&client_app, &mut server);
    let server_out = drain_outputs(&mut server);
    assert_eq!(
        server_out.app_data,
        vec![b"client-app-data".to_vec()],
        "server should receive the client's application data"
    );

    // Replay the stale plaintext ClientHello at the now-confirmed server.
    server
        .handle_packet(&stale_epoch0_handshake)
        .expect("stale ClientHello must be tolerated");
    let resend = collect_packets(&mut server);

    assert!(
        resend.is_empty(),
        "server must not resend its flight after the client is confirmed connected"
    );
}

#[cfg(feature = "rcgen")]
fn forged_epoch1_app_data() -> Vec<u8> {
    // A DTLS 1.2 record header advertising epoch-1 ApplicationData with a
    // garbage (undecryptable) body. The content type lives in the cleartext
    // header, so this looks like app data before anything is decrypted.
    let mut rec = Vec::new();
    rec.push(23); // ContentType::ApplicationData
    rec.extend_from_slice(&[0xFE, 0xFD]); // DTLS 1.2
    rec.extend_from_slice(&1u16.to_be_bytes()); // epoch 1
    rec.extend_from_slice(&[0, 0, 0, 0, 0, 1]); // 48-bit sequence number
    rec.extend_from_slice(&2u16.to_be_bytes()); // fragment length
    rec.extend_from_slice(&[0xAA, 0xBB]); // garbage fragment
    rec
}

/// A forged epoch-1 application-data record arriving before peer encryption is
/// enabled must not confirm the peer handshake. The record advertises
/// ApplicationData in its cleartext header and is queued undecrypted, so without
/// the authentication guard it would falsely mark the peer confirmed and suppress
/// the flight-6 resend that a genuinely lost flight needs.
#[test]
#[cfg(feature = "rcgen")]
fn dtls12_forged_epoch1_appdata_before_encryption_does_not_confirm_peer() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let mut now = Instant::now();
    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");
    let config_client = Arc::new(Config::builder().mtu(115).build().expect("config"));
    let config_server = Arc::new(Config::builder().mtu(115).build().expect("config"));

    let mut client = Dtls::new_12(config_client, client_cert, now);
    client.set_active(true);
    let mut server = Dtls::new_12(config_server, server_cert, now);
    server.set_active(false);

    // Flight 1: ClientHello.
    client.handle_timeout(now).expect("client timeout start");
    client.handle_timeout(now).expect("client arm flight 1");
    let f1 = collect_packets(&mut client);
    for packet in &f1 {
        server.handle_packet(packet).expect("server recv f1");
    }

    // Flight 2: HelloVerifyRequest.
    server.handle_timeout(now).expect("server arm flight 2");
    let f2 = collect_packets(&mut server);
    assert!(!f2.is_empty(), "server should emit flight 2");
    for packet in &f2 {
        client.handle_packet(packet).expect("client recv f2");
    }

    // Flight 3: ClientHello with cookie.
    client.handle_timeout(now).expect("client arm flight 3");
    let f3 = collect_packets(&mut client);
    assert!(!f3.is_empty(), "client should emit flight 3");
    for packet in &f3 {
        server.handle_packet(packet).expect("server recv f3");
    }

    // Flight 4: server hello flight.
    server.handle_timeout(now).expect("server arm flight 4");
    let f4 = collect_packets(&mut server);
    assert!(!f4.is_empty(), "server should emit flight 4");
    for packet in &f4 {
        client.handle_packet(packet).expect("client recv f4");
    }

    // Inject the forged epoch-1 app-data record while the server still has peer
    // encryption disabled (it has not seen the client CCS yet).
    let _ = server.handle_packet(&forged_epoch1_app_data());
    let _ = drain_outputs(&mut server);

    // Flight 5: CKE, CCS, Finished. Server completes and emits flight 6.
    client.handle_timeout(now).expect("client arm flight 5");
    let f5 = collect_packets(&mut client);
    assert!(!f5.is_empty(), "client should emit flight 5");
    for packet in &f5 {
        server.handle_packet(packet).expect("server recv f5");
    }
    server.handle_timeout(now).expect("server arm flight 6");
    let f6_init = collect_packets(&mut server);
    assert!(!f6_init.is_empty(), "server should emit flight 6");

    // Drop flight 6. The client retransmits flight 5 on timeout.
    trigger_timeout(&mut client, &mut now);
    let f5_resend = collect_packets(&mut client);
    assert!(!f5_resend.is_empty(), "client should resend flight 5");
    for packet in &f5_resend {
        server.handle_packet(packet).expect("server recv f5 resend");
    }

    // The forged record must not have confirmed the peer, so the duplicate
    // flight 5 still drives a flight-6 resend.
    let f6_resend = collect_packets(&mut server);
    assert!(
        !f6_resend.is_empty(),
        "forged pre-encryption app data must not confirm the peer; flight 6 must still resend"
    );
}

/// A byte-identical duplicate of an epoch-1 datagram that arrives before the
/// server has processed the client CCS must be dropped, not panic. Before peer
/// encryption is enabled, epoch-1 records are queued undecrypted and bypass the
/// replay window, so the duplicate reaches the receive queue. Here the first
/// flight-5 datagram is lost, which holds the server short of the CCS while the
/// Finished datagram is parked; a network-duplicated copy of it then arrives.
#[test]
#[cfg(feature = "rcgen")]
fn dtls12_duplicate_epoch1_before_ccs_is_dropped() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let now = Instant::now();
    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");
    // Small MTU so the epoch-1 Finished leads its own datagram.
    let config_client = Arc::new(Config::builder().mtu(115).build().expect("config"));
    let config_server = Arc::new(Config::builder().mtu(115).build().expect("config"));

    let mut client = Dtls::new_12(config_client, client_cert, now);
    client.set_active(true);
    let mut server = Dtls::new_12(config_server, server_cert, now);
    server.set_active(false);

    // Poll after every packet and retain any response emitted during polling.
    let deliver_flight = |endpoint: &mut Dtls, flight: &[Vec<u8>]| {
        let mut response = Vec::new();
        for packet in flight {
            endpoint.handle_packet(packet).expect("receive flight");
            response.extend(collect_packets(endpoint));
        }
        response
    };

    server
        .handle_timeout(now)
        .expect("initialize server random");
    assert!(collect_packets(&mut server).is_empty());

    // Flights 1-5: ClientHello, HelloVerifyRequest, ClientHello with cookie,
    // server hello flight, then Certificate/CKE/CertificateVerify/CCS/Finished.
    client.handle_timeout(now).expect("client timeout start");
    let f1 = collect_packets(&mut client);
    assert!(!f1.is_empty(), "client should emit flight 1");
    let f2 = deliver_flight(&mut server, &f1);
    assert!(!f2.is_empty(), "server should emit flight 2");
    let f3 = deliver_flight(&mut client, &f2);
    assert!(!f3.is_empty(), "client should emit flight 3");
    let f4 = deliver_flight(&mut server, &f3);
    assert!(!f4.is_empty(), "server should emit flight 4");
    let f5 = deliver_flight(&mut client, &f4);
    let finished = f5
        .iter()
        .find(|d| parse_records(d).first().map(|h| h.epoch) == Some(1))
        .expect("flight 5 should have a datagram led by the epoch-1 Finished")
        .clone();

    // Lose the first flight-5 datagram. The server cannot reach the CCS, so the
    // Finished stays queued undecrypted.
    for packet in &f5[1..] {
        server.handle_packet(packet).expect("server recv f5");
        drain_outputs(&mut server);
    }

    // A byte-identical duplicate of the Finished datagram arrives.
    server
        .handle_packet(&finished)
        .expect("server recv duplicate Finished");
    drain_outputs(&mut server);

    // The client retransmits flight 5 and the handshake completes. Drain each
    // mutation immediately, retaining responses and connection notifications.
    let mut pending = VecDeque::new();
    let mut connected = [false; 2];
    for attempt in 1..=60 {
        let now = now + Duration::from_secs(attempt);
        for (index, endpoint) in [&mut client, &mut server].into_iter().enumerate() {
            endpoint.handle_timeout(now).expect("recovery timeout");
            let output = drain_outputs(endpoint);
            connected[index] |= output.connected;
            pending.extend(output.packets.into_iter().map(|packet| (1 - index, packet)));
        }
        while let Some((index, packet)) = pending.pop_front() {
            let endpoint = if index == 0 { &mut client } else { &mut server };
            endpoint.handle_packet(&packet).expect("recovery packet");
            let output = drain_outputs(endpoint);
            connected[index] |= output.connected;
            pending.extend(output.packets.into_iter().map(|packet| (1 - index, packet)));
        }
        if connected.iter().all(|connected| *connected) {
            return;
        }
    }
    panic!("handshake should complete after retransmission");
}
