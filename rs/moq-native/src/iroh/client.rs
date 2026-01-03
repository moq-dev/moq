use crate::iroh::EndpointConfig;
use anyhow::{Context, Result};
use url::Url;
use web_transport_iroh::{
	iroh::{Endpoint, EndpointId},
	Session,
};

#[derive(Clone)]
pub struct Client {
	endpoint: Endpoint,
	// TODO: add back support for custom transport config
	// pub transport: Arc<quinn::TransportConfig>,
}

impl Client {
	pub async fn new(config: EndpointConfig) -> Result<Self> {
		let endpoint = config.bind().await?;
		Ok(Self { endpoint })
	}

	pub async fn connect(&self, url: Url) -> Result<Session> {
		let alpn = match url.scheme() {
			"moql+iroh" | "iroh" => moq_lite::lite::ALPN,
			"moqt+iroh" => moq_lite::ietf::ALPN,
			"h3+iroh" => web_transport_iroh::ALPN_H3,
			_ => anyhow::bail!("Invalid URL: unknown scheme"),
		};
		let host = url.host().context("Invalid URL: missing host")?.to_string();
		let endpoint_id: EndpointId = host.parse().context("Invalid URL: host is not an iroh endpoint id")?;
		let conn = self.endpoint.connect(endpoint_id, alpn.as_bytes()).await?;
		let session = match alpn {
			web_transport_iroh::ALPN_H3 => Session::connect_h3(conn, url).await?,
			_ => Session::raw(conn),
		};
		Ok(session)
	}

	pub async fn close(&self) {
		self.endpoint.close().await
	}
}
