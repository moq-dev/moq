/// Default HMAC-SHA1 provider using the `hmac` and `sha1` crates.
///
/// Available when the `sha1` feature is enabled (on by default).
#[cfg(feature = "sha1")]
#[derive(Debug)]
pub struct DefaultSha1HmacProvider;

#[cfg(feature = "sha1")]
impl str0m_proto::Sha1HmacProvider for DefaultSha1HmacProvider {
    fn sha1_hmac(&self, key: &[u8], payloads: &[&[u8]]) -> [u8; 20] {
        use hmac::{Hmac, KeyInit, Mac};
        let mut mac =
            Hmac::<sha1_::Sha1>::new_from_slice(key).expect("HMAC-SHA1 accepts any key length");
        for p in payloads {
            mac.update(p);
        }
        mac.finalize().into_bytes().into()
    }
}
