use rind::{digest, hmac};

use crate::vendor::str0m_proto::crypto::{Sha1HmacProvider, Sha1Provider, Sha256Provider};

pub(super) static SHA1_HMAC: RindSha1Hmac = RindSha1Hmac;
pub(super) static SHA1: RindSha1 = RindSha1;
pub(super) static SHA256: RindSha256 = RindSha256;

#[derive(Debug)]
pub(super) struct RindSha1Hmac;

impl Sha1HmacProvider for RindSha1Hmac {
	fn sha1_hmac(&self, key: &[u8], payloads: &[&[u8]]) -> [u8; 20] {
		let key = hmac::Key::new(&hmac::legacy::HMAC_SHA1, key);
		let mut ctx = hmac::Context::with_key(&key);
		for payload in payloads {
			ctx.update(payload);
		}
		ctx.sign().as_ref().try_into().unwrap()
	}
}

#[derive(Debug)]
pub(super) struct RindSha1;

impl Sha1Provider for RindSha1 {
	fn sha1(&self, data: &[u8]) -> [u8; 20] {
		digest::digest(&digest::legacy::SHA1, data).as_ref().try_into().unwrap()
	}
}

#[derive(Debug)]
pub(super) struct RindSha256;

impl Sha256Provider for RindSha256 {
	fn sha256(&self, data: &[u8]) -> [u8; 32] {
		digest::digest(&digest::SHA256, data).as_ref().try_into().unwrap()
	}
}
