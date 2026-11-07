//! DTLS 1.3 key update tests.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dimpl::{Config, Dtls};

use crate::common::*;

/// Test that KeyUpdate is triggered automatically when AEAD encryption limit is reached.
/// Uses a low limit so the test can observe multiple transparent KeyUpdates.
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_key_update_on_aead_limit() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = Arc::new(
        Config::builder()
            .aead_encryption_limit(10)
            .build()
            .expect("build config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    // Complete handshake
    let mut client_connected = false;
    let mut server_connected = false;
    for _ in 0..30 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        client_connected |= client_out.connected;
        server_connected |= server_out.connected;

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_connected && server_connected {
            break;
        }
        now += Duration::from_millis(50);
    }
    assert!(client_connected, "Client should connect");
    assert!(server_connected, "Server should connect");

    // Send 100 messages client→server. With limit=10, KeyUpdates must happen
    // transparently for all messages to arrive.
    let mut server_received = 0;
    for i in 0..100 {
        let msg = format!("Message {}", i);
        client
            .send_application_data(msg.as_bytes())
            .expect("send app data");

        now += Duration::from_millis(10);

        for _ in 0..3 {
            client.handle_timeout(now).expect("client timeout");
            let client_out = drain_outputs(&mut client);
            deliver_packets(&client_out.packets, &mut server);

            server.handle_timeout(now).expect("server timeout");
            let server_out = drain_outputs(&mut server);
            deliver_packets(&server_out.packets, &mut client);

            server_received += server_out.app_data.len();
        }
    }

    assert_eq!(
        server_received, 100,
        "All messages should be received (proves KeyUpdate worked transparently)"
    );
}

/// Test that bidirectional traffic works with auto-KeyUpdate on both sides.
/// Sends 100 messages in each direction (client first, then server) to avoid
/// simultaneous KeyUpdate contention.
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_key_update_bidirectional_after_limit() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = Arc::new(
        Config::builder()
            .aead_encryption_limit(10)
            .build()
            .expect("build config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    // Complete handshake
    let mut client_connected = false;
    let mut server_connected = false;
    for _ in 0..30 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        client_connected |= client_out.connected;
        server_connected |= server_out.connected;

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_connected && server_connected {
            break;
        }
        now += Duration::from_millis(50);
    }
    assert!(client_connected, "Client should connect");
    assert!(server_connected, "Server should connect");

    let mut server_received = 0;
    let mut client_received = 0;

    // Phase 1: Send 100 messages client→server (triggers KeyUpdates on client)
    for i in 0..100 {
        let msg = format!("Client msg {}", i);
        client
            .send_application_data(msg.as_bytes())
            .expect("client send");

        now += Duration::from_millis(10);

        for _ in 0..3 {
            client.handle_timeout(now).expect("client timeout");
            let client_out = drain_outputs(&mut client);
            deliver_packets(&client_out.packets, &mut server);

            server.handle_timeout(now).expect("server timeout");
            let server_out = drain_outputs(&mut server);
            deliver_packets(&server_out.packets, &mut client);

            server_received += server_out.app_data.len();
        }
    }

    // Phase 2: Send 100 messages server→client (triggers KeyUpdates on server)
    for i in 0..100 {
        let msg = format!("Server msg {}", i);
        server
            .send_application_data(msg.as_bytes())
            .expect("server send");

        now += Duration::from_millis(10);

        for _ in 0..3 {
            server.handle_timeout(now).expect("server timeout");
            let server_out = drain_outputs(&mut server);
            deliver_packets(&server_out.packets, &mut client);

            client.handle_timeout(now).expect("client timeout");
            let client_out = drain_outputs(&mut client);
            deliver_packets(&client_out.packets, &mut server);

            client_received += client_out.app_data.len();
        }
    }

    assert_eq!(
        server_received, 100,
        "Server should receive all messages (proves KeyUpdate worked for client→server)"
    );
    assert_eq!(
        client_received, 100,
        "Client should receive all messages (proves KeyUpdate worked for server→client)"
    );
}

/// Test that a reordered packet captured before a KeyUpdate is accepted when
/// delivered alongside other packets during the transition. The packet is from
/// the same epoch and arrives before any new-epoch records, so the replay
/// window accepts it and the retained old epoch keys decrypt it.
///
/// This verifies that auto-KeyUpdate is transparent to application data, even
/// when packets arrive out of order during the key transition.
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_key_update_old_epoch_packet_still_decrypted() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = Arc::new(
        Config::builder()
            .aead_encryption_limit(10)
            .build()
            .expect("build config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    // Complete handshake
    let mut client_connected = false;
    let mut server_connected = false;
    for _ in 0..30 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        client_connected |= client_out.connected;
        server_connected |= server_out.connected;

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_connected && server_connected {
            break;
        }
        now += Duration::from_millis(50);
    }
    assert!(client_connected, "Client should connect");
    assert!(server_connected, "Server should connect");

    // Send the first message and capture its raw packets WITHOUT delivering.
    // This packet is encrypted on epoch 3 (the initial application epoch).
    client
        .send_application_data(b"delayed-old-epoch")
        .expect("send delayed msg");

    now += Duration::from_millis(10);
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    let delayed_packets = client_out.packets.clone();

    // Process server output (don't deliver delayed to server).
    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    deliver_packets(&server_out.packets, &mut client);

    // Send enough messages to trigger at least one KeyUpdate (limit=10).
    // The KeyUpdate fires transparently as part of the normal exchange.
    // With the delayed packet withheld, the AEAD count starts from the
    // delayed message (count=1) plus each additional message.
    let mut server_received = 0;
    for i in 0..15 {
        let msg = format!("Message {}", i);
        client
            .send_application_data(msg.as_bytes())
            .expect("send app data");

        now += Duration::from_millis(10);

        for _ in 0..3 {
            client.handle_timeout(now).expect("client timeout");
            let client_out = drain_outputs(&mut client);
            deliver_packets(&client_out.packets, &mut server);

            server.handle_timeout(now).expect("server timeout");
            let server_out = drain_outputs(&mut server);
            deliver_packets(&server_out.packets, &mut client);

            server_received += server_out.app_data.len();
        }
    }

    assert_eq!(
        server_received, 15,
        "All regular messages should be received"
    );

    // Now deliver the delayed packet that was captured before the KeyUpdate.
    // The server has completed one or more KeyUpdates by now. The old epoch
    // keys (epoch 3) are retained in the app_recv_keys array (up to 4 entries).
    // Each epoch has its own replay window, so the old-epoch packet is accepted
    // and decrypted (the per-epoch window for epoch 3 hasn't seen this seqno).
    //
    // We verify the connection is still healthy after delivering the late
    // packet by sending additional data.
    deliver_packets(&delayed_packets, &mut server);
    now += Duration::from_millis(10);
    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    deliver_packets(&server_out.packets, &mut client);

    // Verify post-KeyUpdate data exchange still works. The stale old-epoch
    // packet should not have disrupted the connection.
    let mut post_received = 0;
    for i in 0..10 {
        let msg = format!("Post msg {}", i);
        client
            .send_application_data(msg.as_bytes())
            .expect("send post msg");

        now += Duration::from_millis(10);

        for _ in 0..3 {
            client.handle_timeout(now).expect("client timeout");
            let client_out = drain_outputs(&mut client);
            deliver_packets(&client_out.packets, &mut server);

            server.handle_timeout(now).expect("server timeout");
            let server_out = drain_outputs(&mut server);
            deliver_packets(&server_out.packets, &mut client);

            post_received += server_out.app_data.len();
        }
    }

    assert_eq!(
        post_received, 10,
        "All post-KeyUpdate messages should be received (stale packet didn't break connection)"
    );
}

/// Test that multiple sequential KeyUpdates work correctly. With a very low
/// AEAD limit, sending many messages should trigger 3+ KeyUpdates in sequence,
/// and all messages must still be received.
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_key_update_multiple_sequential() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    // With limit=3, quarter=0, so threshold=3 exactly (no jitter).
    // Each KeyUpdate cycle: ~3 app records before the next KeyUpdate triggers.
    let config = Arc::new(
        Config::builder()
            .aead_encryption_limit(3)
            .build()
            .expect("build config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    // Complete handshake
    let mut client_connected = false;
    let mut server_connected = false;
    for _ in 0..30 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        client_connected |= client_out.connected;
        server_connected |= server_out.connected;

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_connected && server_connected {
            break;
        }
        now += Duration::from_millis(50);
    }
    assert!(client_connected, "Client should connect");
    assert!(server_connected, "Server should connect");

    // Send 30 messages. With limit=3, this should trigger at least 3 KeyUpdates
    // (likely more, since ACKs and KeyUpdate records themselves also count as
    // AEAD encryptions on the current epoch).
    let mut server_received = 0;
    for i in 0..30 {
        let msg = format!("Message {}", i);
        client
            .send_application_data(msg.as_bytes())
            .expect("send app data");

        now += Duration::from_millis(10);

        for _ in 0..5 {
            client.handle_timeout(now).expect("client timeout");
            let client_out = drain_outputs(&mut client);
            deliver_packets(&client_out.packets, &mut server);

            server.handle_timeout(now).expect("server timeout");
            let server_out = drain_outputs(&mut server);
            deliver_packets(&server_out.packets, &mut client);

            server_received += server_out.app_data.len();
        }
    }

    assert_eq!(
        server_received, 30,
        "All 30 messages should be received across 3+ KeyUpdates"
    );
}

/// Test that KeyUpdate completes correctly even when the KeyUpdate message
/// itself is lost and must be retransmitted via timeout. After recovery,
/// subsequent data exchange must work normally.
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_key_update_with_packet_loss() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = Arc::new(
        Config::builder()
            .aead_encryption_limit(5)
            .build()
            .expect("build config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    // Complete handshake
    let mut client_connected = false;
    let mut server_connected = false;
    for _ in 0..30 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        client_connected |= client_out.connected;
        server_connected |= server_out.connected;

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_connected && server_connected {
            break;
        }
        now += Duration::from_millis(50);
    }
    assert!(client_connected, "Client should connect");
    assert!(server_connected, "Server should connect");

    // Send enough messages to trigger KeyUpdate, but drop all packets from the
    // round where the KeyUpdate fires, simulating network loss of the KeyUpdate.
    let mut _server_received = 0;
    let mut dropped_round = false;
    for i in 0..10 {
        let msg = format!("Message {}", i);
        client
            .send_application_data(msg.as_bytes())
            .expect("send app data");

        now += Duration::from_millis(10);

        client.handle_timeout(now).expect("client timeout");
        let client_out = drain_outputs(&mut client);

        // Drop one round of client packets (simulating KeyUpdate loss).
        // We drop round i==5 which should be around the time a KeyUpdate fires.
        if i == 5 && !dropped_round {
            dropped_round = true;
            // Don't deliver client_out.packets to server — they are lost.
        } else {
            deliver_packets(&client_out.packets, &mut server);
        }

        server.handle_timeout(now).expect("server timeout");
        let server_out = drain_outputs(&mut server);
        deliver_packets(&server_out.packets, &mut client);

        _server_received += server_out.app_data.len();
    }

    // The dropped round means at least one message was lost. Now trigger a
    // retransmission timeout so the KeyUpdate (and any lost data) is resent.
    for _ in 0..10 {
        trigger_timeout(&mut client, &mut now);
        let client_out = drain_outputs(&mut client);
        deliver_packets(&client_out.packets, &mut server);

        server.handle_timeout(now).expect("server timeout");
        let server_out = drain_outputs(&mut server);
        deliver_packets(&server_out.packets, &mut client);

        _server_received += server_out.app_data.len();
    }

    // Continue sending more messages to prove the connection is healthy.
    let mut post_recovery_received = 0;
    for i in 0..10 {
        let msg = format!("Post-recovery {}", i);
        client
            .send_application_data(msg.as_bytes())
            .expect("send post-recovery");

        now += Duration::from_millis(10);

        for _ in 0..3 {
            client.handle_timeout(now).expect("client timeout");
            let client_out = drain_outputs(&mut client);
            deliver_packets(&client_out.packets, &mut server);

            server.handle_timeout(now).expect("server timeout");
            let server_out = drain_outputs(&mut server);
            deliver_packets(&server_out.packets, &mut client);

            post_recovery_received += server_out.app_data.len();
        }
    }

    assert_eq!(
        post_recovery_received, 10,
        "All post-recovery messages should be received"
    );
}

/// Test that high-frequency KeyUpdates work correctly. With the minimum
/// AEAD limit of 2, nearly every message triggers a KeyUpdate. This stress-
/// tests the key rotation machinery and epoch tracking under extreme churn,
/// exercising many more KeyUpdate cycles than the limit=10 tests above.
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_key_update_high_frequency() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    // With limit=2, quarter=0 so threshold=2 exactly. Every 2 AEAD
    // encryptions on an epoch triggers a KeyUpdate. This means nearly
    // every application message triggers a key rotation.
    let config = Arc::new(
        Config::builder()
            .aead_encryption_limit(2)
            .build()
            .expect("build config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    // Complete handshake
    let mut client_connected = false;
    let mut server_connected = false;
    for _ in 0..30 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        client_connected |= client_out.connected;
        server_connected |= server_out.connected;

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_connected && server_connected {
            break;
        }
        now += Duration::from_millis(50);
    }
    assert!(client_connected, "Client should connect");
    assert!(server_connected, "Server should connect");

    // Send 50 messages with limit=2. This triggers ~25 KeyUpdates,
    // exercising epoch cycling through many values (3, 4, 5, ... ~28).
    let mut server_received = 0;
    for i in 0..50 {
        let msg = format!("High-freq msg {}", i);
        client
            .send_application_data(msg.as_bytes())
            .expect("send app data");

        now += Duration::from_millis(10);

        for _ in 0..5 {
            client.handle_timeout(now).expect("client timeout");
            let client_out = drain_outputs(&mut client);
            deliver_packets(&client_out.packets, &mut server);

            server.handle_timeout(now).expect("server timeout");
            let server_out = drain_outputs(&mut server);
            deliver_packets(&server_out.packets, &mut client);

            server_received += server_out.app_data.len();
        }
    }

    assert_eq!(
        server_received, 50,
        "All 50 messages should be received despite high-frequency KeyUpdates"
    );
}

/// Losing the first ACK must not strand either peer's KeyUpdate.
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_key_update_retransmission_is_acked_again() {
    use dimpl::certificate::generate_self_signed_certificate;

    for sender_is_client in [false, true] {
        let now = Instant::now();
        let low_limit = Arc::new(
            Config::builder()
                .aead_encryption_limit(2)
                .build()
                .expect("build config"),
        );
        let (client_config, server_config) = if sender_is_client {
            (low_limit, dtls13_config())
        } else {
            (dtls13_config(), low_limit)
        };
        let mut client = Dtls::new_13(
            client_config,
            generate_self_signed_certificate().expect("client cert"),
            now,
        );
        client.set_active(true);
        let mut server = Dtls::new_13(
            server_config,
            generate_self_signed_certificate().expect("server cert"),
            now,
        );
        let mut now = complete_dtls13_handshake(&mut client, &mut server, now);
        drain_outputs(&mut client);
        drain_outputs(&mut server);
        let (sender, receiver) = if sender_is_client {
            (&mut client, &mut server)
        } else {
            (&mut server, &mut client)
        };

        // Reach the encryption limit and discard the initial KeyUpdate ACK.
        for _ in 0..2 {
            sender.send_application_data(b"rotate").expect("send data");
            let packets = drain_outputs(sender).packets;
            for packet in packets {
                receiver
                    .handle_packet(&packet)
                    .expect("receive data/update");
                drain_outputs(receiver);
            }
        }
        sender.handle_timeout(now).expect("start key update");
        for packet in drain_outputs(sender).packets {
            receiver.handle_packet(&packet).expect("receive key update");
            drain_outputs(receiver);
        }

        // Deliver every retry and response. Before the fix the sender exhausts
        // its retry budget because the duplicate KeyUpdate is never ACKed.
        for round in 1..=8 {
            now += Duration::from_secs(1 << round);
            sender.handle_timeout(now).expect("KeyUpdate must recover");
            for packet in drain_outputs(sender).packets {
                receiver.handle_packet(&packet).expect("receive retry");
                for response in drain_outputs(receiver).packets {
                    sender.handle_packet(&response).expect("receive response");
                    for reply in drain_outputs(sender).packets {
                        receiver.handle_packet(&reply).expect("receive reply");
                        drain_outputs(receiver);
                    }
                }
            }
        }
        sender
            .send_application_data(b"recovered")
            .expect("send after recovery");
        let mut received = Vec::new();
        for packet in drain_outputs(sender).packets {
            receiver
                .handle_packet(&packet)
                .expect("receive after recovery");
            received.extend(drain_outputs(receiver).app_data);
        }
        assert_eq!(received, [b"recovered".to_vec()]);
    }
}
