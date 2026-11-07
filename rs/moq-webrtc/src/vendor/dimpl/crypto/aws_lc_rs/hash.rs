//! Hash implementations using aws-lc-rs.

use aws_lc_rs::digest::{Context, SHA256, SHA384};

use super::super::{HashContext, HashProvider};
use crate::buffer::Buf;
use crate::types::HashAlgorithm;

/// Hash context implementation using aws-lc-rs.
struct AwsLcHashContext {
    context: Context,
}

impl std::fmt::Debug for AwsLcHashContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwsLcHashContext").finish_non_exhaustive()
    }
}

impl HashContext for AwsLcHashContext {
    fn update(&mut self, data: &[u8]) {
        self.context.update(data);
    }

    fn clone_and_finalize(&self, out: &mut Buf) {
        let digest = self.context.clone().finish();
        out.clear();
        out.extend_from_slice(digest.as_ref());
    }
}

/// Hash provider implementation.
#[derive(Debug)]
pub(super) struct AwsLcHashProvider;

impl HashProvider for AwsLcHashProvider {
    fn create_hash(&self, algorithm: HashAlgorithm) -> Box<dyn HashContext> {
        let context = match algorithm {
            HashAlgorithm::SHA256 => Context::new(&SHA256),
            HashAlgorithm::SHA384 => Context::new(&SHA384),
            _ => panic!("Unsupported hash algorithm: {:?}", algorithm),
        };
        Box::new(AwsLcHashContext { context })
    }
}

/// Static instance of the hash provider.
pub(super) static HASH_PROVIDER: AwsLcHashProvider = AwsLcHashProvider;
