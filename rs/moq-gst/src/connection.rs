//! Connection properties shared by publishers and subscribers.

use gst::glib;
use gst::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, Default)]
pub(crate) struct Settings {
	version: Option<String>,
	tls_disable_verify: bool,
	quic_idle_timeout: Option<Duration>,
	quic_keep_alive: Option<Duration>,
	quic_keylog: bool,
}

pub(crate) fn duration_millis(duration: Duration) -> u64 {
	duration.as_millis().try_into().unwrap_or(u64::MAX)
}

pub(crate) fn properties() -> Vec<glib::ParamSpec> {
	let quic = moq_tokio::quic::Resolved::default();
	vec![
		glib::ParamSpecString::builder("connect-version")
			.nick("Connect version")
			.blurb(
				"Comma-separated protocol versions to offer, most preferred first; unset offers every stable version",
			)
			.mutable_ready()
			.build(),
		glib::ParamSpecBoolean::builder("tls-disable-verify")
			.nick("TLS disable verify")
			.blurb("Disable TLS certificate verification")
			.mutable_ready()
			.build(),
		glib::ParamSpecUInt64::builder("quic-idle-timeout")
			.nick("QUIC idle timeout")
			.blurb("QUIC idle timeout in milliseconds, 0 to disable locally")
			.default_value(duration_millis(quic.idle_timeout))
			.mutable_ready()
			.build(),
		glib::ParamSpecUInt64::builder("quic-keep-alive")
			.nick("QUIC keep-alive")
			.blurb("QUIC keep-alive interval in milliseconds, 0 to disable; unset derives it from the idle timeout; iroh still pings at least every 5s")
			.default_value(quic.keep_alive_interval().map(duration_millis).unwrap_or(0))
			.mutable_ready()
			.build(),
		glib::ParamSpecBoolean::builder("quic-keylog")
			.nick("QUIC TLS key log")
			.blurb("Danger: write TLS session secrets to the file SSLKEYLOGFILE names; without it the start fails")
			.mutable_ready()
			.build(),
	]
}

impl Settings {
	pub(crate) fn set(&mut self, name: &str, value: &glib::Value) {
		match name {
			"connect-version" => self.version = value.get().unwrap(),
			"tls-disable-verify" => self.tls_disable_verify = value.get().unwrap(),
			"quic-idle-timeout" => self.quic_idle_timeout = Some(Duration::from_millis(value.get().unwrap())),
			"quic-keep-alive" => self.quic_keep_alive = Some(Duration::from_millis(value.get().unwrap())),
			"quic-keylog" => self.quic_keylog = value.get().unwrap(),
			_ => unreachable!(),
		}
	}

	pub(crate) fn get(&self, name: &str) -> glib::Value {
		match name {
			"connect-version" => self.version.to_value(),
			"tls-disable-verify" => self.tls_disable_verify.to_value(),
			"quic-idle-timeout" => duration_millis(
				self.quic_idle_timeout
					.unwrap_or_else(|| moq_tokio::quic::Resolved::default().idle_timeout),
			)
			.to_value(),
			// Unset reads as what it derives to against a peer that does not shorten the idle timeout.
			"quic-keep-alive" => self
				.quic_keep_alive
				.or_else(|| {
					let mut quic = moq_tokio::quic::Resolved::default();
					quic.idle_timeout = self.quic_idle_timeout.unwrap_or(quic.idle_timeout);
					quic.keep_alive_interval()
				})
				.map(duration_millis)
				.unwrap_or(0)
				.to_value(),
			"quic-keylog" => self.quic_keylog.to_value(),
			_ => unreachable!(),
		}
	}

	pub(crate) fn resolve(&self) -> anyhow::Result<(moq_tokio::connect::Config, moq_tokio::quic::Config)> {
		let mut connect = moq_tokio::connect::Config::default();
		connect.version = self
			.version
			.iter()
			.flat_map(|versions| versions.split(','))
			.map(str::trim)
			.filter(|version| !version.is_empty())
			.map(|version| version.parse().map_err(anyhow::Error::msg))
			.collect::<anyhow::Result<_>>()?;
		connect.tls.insecure = Some(self.tls_disable_verify);
		connect.backoff.timeout = Some(Duration::ZERO.into());
		let mut quic = moq_tokio::quic::Config::default();
		quic.idle_timeout = self.quic_idle_timeout.map(Into::into);
		quic.keep_alive = self.quic_keep_alive.map(Into::into);
		quic.keylog = self
			.quic_keylog
			.then(|| std::env::var_os("SSLKEYLOGFILE").unwrap_or_default().into());
		Ok((connect, quic))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn both_elements_parse_the_same_connection_properties() {
		gst::init().unwrap();
		let elements: [gst::Element; 2] = [
			glib::Object::new::<crate::sink::MoqSink>().upcast(),
			glib::Object::new::<crate::source::MoqSrc>().upcast(),
		];
		for element in elements {
			assert!(element.find_property("versions").is_none());
			for prop in properties() {
				let actual = element.find_property(prop.name()).unwrap();
				assert_eq!(actual.value_type(), prop.value_type());
				assert!(actual.flags().contains(gst::PARAM_FLAG_MUTABLE_READY));
			}
			for (name, value) in [("quic-idle-timeout", 15_000u64), ("quic-keep-alive", 0u64)] {
				element.set_property(name, value);
				assert_eq!(element.property::<u64>(name), value);
			}
			element.set_property("connect-version", " moq-transport-18 , moq-lite-06 ");
			assert_eq!(
				element.property::<String>("connect-version"),
				" moq-transport-18 , moq-lite-06 "
			);
		}
	}

	/// An unset keep-alive reads as what it derives to, following the idle timeout.
	#[test]
	fn unset_keep_alive_reads_as_derived() {
		let mut settings = Settings::default();
		assert_eq!(settings.get("quic-keep-alive").get::<u64>().unwrap(), 3_000);
		settings.set("quic-idle-timeout", &30_000u64.to_value());
		assert_eq!(settings.get("quic-keep-alive").get::<u64>().unwrap(), 9_000);
		assert_eq!(settings.resolve().unwrap().1.resolve().keep_alive, None);
	}

	#[test]
	fn properties_reach_versions_tls_and_quic_config() {
		let mut settings = Settings::default();
		for (name, value) in [
			("connect-version", " moq-transport-18 , moq-lite-06 ".to_value()),
			("tls-disable-verify", true.to_value()),
			("quic-idle-timeout", 15_000u64.to_value()),
			("quic-keep-alive", 0u64.to_value()),
			("quic-keylog", true.to_value()),
		] {
			settings.set(name, &value);
		}
		let (connect, quic) = settings.resolve().unwrap();
		assert_eq!(
			connect.version,
			["moq-transport-18".parse().unwrap(), "moq-lite-06".parse().unwrap()]
		);
		assert_eq!(connect.tls.insecure, Some(true));
		assert_eq!(connect.backoff.timeout, Some(Duration::ZERO.into()));
		let quic = quic.resolve();
		assert_eq!(quic.idle_timeout, Duration::from_secs(15));
		assert_eq!(quic.keep_alive, Some(Duration::ZERO));
		assert!(quic.keylog.is_some());
		settings.set("connect-version", &"moq-lite-99".to_value());
		assert!(settings.resolve().unwrap_err().to_string().contains("moq-lite-99"));
	}
}
