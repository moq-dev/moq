//! Shared helpers for auto-negotiation integration tests.

#![allow(unused)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use dimpl::{Config, Dtls, Output, SrtpProfile};

/// Collected outputs from polling an endpoint to `Timeout`.
#[derive(Default, Debug)]
pub struct DrainedOutputs {
    pub packets: Vec<Vec<u8>>,
    pub connected: bool,
    pub peer_cert: Option<Vec<u8>>,
    pub keying_material: Option<(Vec<u8>, SrtpProfile)>,
    pub app_data: Vec<Vec<u8>>,
    pub timeout: Option<Instant>,
    pub close_notify: bool,
}

/// Poll until `Timeout`, collecting only packets.
pub fn collect_packets(endpoint: &mut Dtls) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; 2048];
    loop {
        match endpoint.poll_output(&mut buf) {
            Output::Packet(p) => out.push(p.to_vec()),
            Output::Timeout(_) => break,
            _ => {}
        }
    }
    out
}

/// Poll until `Timeout`, collecting everything.
pub fn drain_outputs(endpoint: &mut Dtls) -> DrainedOutputs {
    let mut result = DrainedOutputs::default();
    let mut buf = vec![0u8; 2048];
    loop {
        match endpoint.poll_output(&mut buf) {
            Output::Packet(p) => result.packets.push(p.to_vec()),
            Output::Connected => result.connected = true,
            Output::PeerCert(cert) => result.peer_cert = Some(cert.to_vec()),
            Output::KeyingMaterial(km, profile) => {
                result.keying_material = Some((km.to_vec(), profile));
            }
            Output::ApplicationData(data) => result.app_data.push(data.to_vec()),
            Output::CloseNotify => result.close_notify = true,
            Output::Timeout(t) => {
                result.timeout = Some(t);
                break;
            }
            _ => {}
        }
    }
    result
}

/// Deliver a slice of packets to a destination endpoint.
pub fn deliver_packets(packets: &[Vec<u8>], dest: &mut Dtls) {
    for p in packets {
        let _ = dest.handle_packet(p);
    }
}

/// Create a default config.
pub fn default_config() -> Arc<Config> {
    Arc::new(Config::builder().build().expect("Failed to build config"))
}

/// Create a config with the server cookie exchange disabled.
pub fn no_cookie_config() -> Arc<Config> {
    Arc::new(
        Config::builder()
            .use_server_cookie(false)
            .build()
            .expect("Failed to build config"),
    )
}

/// Create a config with a small MTU to force ClientHello fragmentation.
pub fn small_mtu_config(mtu: usize) -> Arc<Config> {
    Arc::new(
        Config::builder()
            .mtu(mtu)
            .build()
            .expect("Failed to build config"),
    )
}
