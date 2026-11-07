/// HMAC-SHA1 for the ICE agent's own tests, from the aws-lc-rs provider str0m runs on.
#[cfg(test)]
#[derive(Debug)]
pub struct DefaultSha1HmacProvider;

#[cfg(test)]
impl crate::vendor::str0m_proto::Sha1HmacProvider for DefaultSha1HmacProvider {
    fn sha1_hmac(&self, key: &[u8], payloads: &[&[u8]]) -> [u8; 20] {
        crate::vendor::str0m_aws_lc_rs::default_provider()
            .sha1_hmac_provider
            .sha1_hmac(key, payloads)
    }
}
