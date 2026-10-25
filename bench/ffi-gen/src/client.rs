//! Connecting to a relay: native only, like moq-ffi's QUIC client. The browser connects
//! through web-transport-wasm, which the slice leaves out.

#[cfg(feature = "boltffi")]
use boltffi::export;
#[cfg(feature = "uniffi")]
use moq_ffi_gen_uniffi_shim::export;

use crate::{MoqError, MoqOriginProducer, runtime};

/// Run `fut` on the worker runtime and await its result from the host's poll.
async fn on_runtime<T: Send + 'static>(fut: impl std::future::Future<Output = T> + Send + 'static) -> T {
	runtime().spawn(fut).await.expect("runtime task panicked")
}

/// Dials relays, publishing and consuming through one origin.
#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct MoqClient {
	client: moq_tokio::Client,
}

#[export]
impl MoqClient {
	/// A client that dials once, with no reconnect.
	#[cfg_attr(feature = "uniffi", uniffi::constructor)]
	pub fn new() -> Result<Self, MoqError> {
		let connect = moq_tokio::connect::Config::default();
		let _guard = runtime().enter();
		let client = connect
			.init(moq_tokio::quic::Config::default())
			.map_err(|err| MoqError::Net {
				reason: err.to_string(),
			})?;
		Ok(Self { client: client.with_reconnect(false) })
	}

	/// Connect to `url`, publishing and consuming through `origin`. Takes `&self`, which
	/// BoltFFI binds.
	pub async fn connect(&self, url: String, origin: &MoqOriginProducer) -> Result<MoqSession, MoqError> {
		let url = url::Url::parse(&url).map_err(|err| MoqError::Net {
			reason: err.to_string(),
		})?;
		let client = self
			.client
			.clone()
			.with_publisher(origin.inner.consume())
			.with_subscriber(origin.inner.clone());
		// `connect` spawns its driver, so it runs on the runtime too.
		let connection = on_runtime(async move { client.connect(url).established().await })
			.await
			.map_err(|err| MoqError::Net {
				reason: err.to_string(),
			})?;
		Ok(MoqSession {
			_connection: connection,
		})
	}
}

/// An established connection, closed on drop.
#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct MoqSession {
	_connection: moq_tokio::Connection,
}

#[export]
impl MoqSession {
	/// Dial `url` with a default client: an async constructor (boltffi/boltffi#95).
	#[cfg_attr(feature = "uniffi", uniffi::constructor)]
	pub async fn dial(url: String, origin: &MoqOriginProducer) -> Result<Self, MoqError> {
		let client = MoqClient::new()?;
		client.connect(url, origin).await
	}
}
