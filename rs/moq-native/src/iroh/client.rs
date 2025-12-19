use crate::iroh::EndpointConfig;
use anyhow::Context;
use url::Url;
use web_transport_iroh::iroh::{Endpoint, EndpointAddr, EndpointId};

#[derive(Clone)]
pub struct Client {
	endpoint: Endpoint,
	// TODO: add back support for custom transport config
	// pub transport: Arc<quinn::TransportConfig>,
}

impl Client {
	pub async fn new(config: EndpointConfig) -> anyhow::Result<Self> {
		let endpoint = config.bind().await?;
		Ok(Self { endpoint })
	}

	pub async fn connect(&self, url: Url) -> anyhow::Result<web_transport_iroh::Session> {
		anyhow::ensure!(url.scheme() == "iroh", "invalid URL: wrong scheme, must be iroh://");
		let host = url.host().context("invalid URL: missing host")?.to_string();
		let endpoint_id: EndpointId = host
			.parse()
			.context("invalid URL: failed to parse host as iroh endpoint id")?;
		let session = self.connect_addr(endpoint_id).await?;
		Ok(session)
	}

	/// Connect to a server.
	pub async fn connect_addr(&self, addr: impl Into<EndpointAddr>) -> anyhow::Result<web_transport_iroh::Session> {
		let addr = addr.into();
		let url: Url = format!("iroh://{}", addr.id).parse().unwrap();
		// Connect to the server using the addr we just resolved.
		let conn = self.endpoint.connect(addr, web_transport_iroh::ALPN.as_bytes()).await?;

		// Connect with the connection we established.
		Ok(web_transport_iroh::Session::raw(conn, url))
	}

	pub async fn close(&self) {
		self.endpoint.close().await
	}
}
