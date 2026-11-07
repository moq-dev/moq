use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use crate::vendor::dimpl::certificate::generate_self_signed_certificate;
use crate::vendor::dimpl::{Config, Dtls};

use super::dtls12::common;
use common::drain_outputs;

#[test]
fn corrected_client_hello_completes_after_malformed_extension() {
	assert_client_hello_recovery(false);
}

#[test]
fn corrected_client_hello_completes_after_malformed_extension_with_cookie() {
	assert_client_hello_recovery(true);
}

fn assert_client_hello_recovery(use_cookie: bool) {
	let certificate = generate_self_signed_certificate().expect("certificate");
	let config = Arc::new(Config::builder().use_server_cookie(use_cookie).build().expect("config"));
	{
		for kind in [10u16, 13, 14] {
			for fragmented in [false, true] {
				let now = Instant::now();
				let mut client = Dtls::new_12(config.clone(), certificate.clone(), now);
				let mut server = Dtls::new_12(config.clone(), certificate.clone(), now);
				client.set_active(true);
				client.handle_timeout(now).expect("client clock");
				let mut hello = drain_outputs(&mut client).packets;
				assert_eq!(hello.len(), 1);
				server.handle_timeout(now).expect("server clock");
				drain_outputs(&mut server);
				if use_cookie {
					let mut challenge = Vec::new();
					for packet in &hello {
						server.handle_packet(packet).expect("initial hello");
						challenge.extend(drain_outputs(&mut server).packets);
					}
					assert!(!challenge.is_empty(), "server cookie challenge");
					hello.clear();
					for packet in challenge {
						client.handle_packet(&packet).expect("cookie challenge");
						hello.extend(drain_outputs(&mut client).packets);
					}
					assert_eq!(hello.len(), 1, "second ClientHello");
					// The legacy cookie follows the session id.
					let cookie_start = 60 + hello[0][59] as usize;
					assert!(
						hello[0][cookie_start] != 0,
						"second ClientHello must include the cookie"
					);
				}
				let mut malformed = hello[0].clone();
				let start = extension_start(&malformed, kind).expect("required test extension");
				malformed[start] = 0xff;
				let chunk = if fragmented {
					(malformed.len() - 25).div_ceil(2)
				} else {
					malformed.len() - 25
				};
				for (index, body) in malformed[25..].chunks(chunk).enumerate() {
					let mut fragment = malformed[..25].to_vec();
					fragment.extend_from_slice(body);
					fragment[5..11].copy_from_slice(&(index as u64).to_be_bytes()[2..]);
					fragment[11..13].copy_from_slice(&((12 + body.len()) as u16).to_be_bytes());
					fragment[19..22].copy_from_slice(&((index * chunk) as u32).to_be_bytes()[1..]);
					fragment[22..25].copy_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
					server.handle_packet(&fragment).expect("discard malformed hello");
					assert!(
						drain_outputs(&mut server).packets.is_empty(),
						"extension {kind}, fragmented={fragmented}, cookie={use_cookie}"
					);
				}
				server.handle_packet(&hello[0]).expect("corrected hello");
				let mut connected = [false; 2];
				let mut pending = VecDeque::new();
				let output = drain_outputs(&mut server);
				connected[1] |= output.connected;
				pending.extend(output.packets.into_iter().map(|packet| (0, packet)));
				for _ in 0..100 {
					let Some((recipient, packet)) = pending.pop_front() else {
						break;
					};
					let target = if recipient == 0 { &mut client } else { &mut server };
					target.handle_packet(&packet).expect("handshake packet");
					let output = drain_outputs(target);
					connected[recipient] |= output.connected;
					pending.extend(output.packets.into_iter().map(|packet| (1 - recipient, packet)));
				}
				assert_eq!(
					connected,
					[true, true],
					"extension {kind}, fragmented={fragmented}, cookie={use_cookie}"
				);
			}
		}
	}
}

fn extension_start(hello: &[u8], kind: u16) -> Option<usize> {
	// Record (13), handshake (12), version (2), random (32).
	let mut cursor = 59;
	cursor += 1 + hello[cursor] as usize; // session id
	cursor += 1 + hello[cursor] as usize; // cookie
	cursor += 2 + u16::from_be_bytes([hello[cursor], hello[cursor + 1]]) as usize;
	cursor += 1 + hello[cursor] as usize; // compression
	cursor += 2; // extensions length
	while cursor < hello.len() {
		let extension = u16::from_be_bytes([hello[cursor], hello[cursor + 1]]);
		let len = u16::from_be_bytes([hello[cursor + 2], hello[cursor + 3]]) as usize;
		if extension == kind {
			return Some(cursor + 4);
		}
		cursor += 4 + len;
	}
	None
}
