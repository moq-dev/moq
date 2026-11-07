use std::fmt;
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::sync::Arc;
use std::time::Duration;

use crate::crypto::{CryptoProvider, SupportedDtls12CipherSuite};
use crate::crypto::{SupportedDtls13CipherSuite, SupportedKxGroup};
use crate::dtls12::message::Dtls12CipherSuite;
use crate::types::{Dtls13CipherSuite, NamedGroup};
use crate::{ConfigError, Error};

const MAX_TIMING_DURATION: Duration = Duration::from_secs(10 * 365 * 24 * 60 * 60);

/// Callback for resolving PSK identities to shared secrets.
///
/// Implement this trait and provide it via [`ConfigBuilder::with_psk_client`]
/// or [`ConfigBuilder::with_psk_server`] to enable PSK cipher suites.
pub trait PskResolver: Send + Sync + UnwindSafe + RefUnwindSafe {
    /// Look up a pre-shared key by the peer's identity.
    ///
    /// Returns the shared secret bytes, or `None` if the identity is unknown.
    fn resolve(&self, identity: &[u8]) -> Option<Vec<u8>>;
}

/// PSK configuration for a DTLS endpoint.
///
/// Use [`Psk::Client`] for endpoints that initiate PSK handshakes (send identity),
/// and [`Psk::Server`] for endpoints that resolve incoming identities.
///
/// `#[non_exhaustive]` so new variants (e.g. DTLS 1.3 external PSKs) or new
/// fields can be added without a major version bump.
#[derive(Clone)]
#[non_exhaustive]
pub enum Psk {
    /// Client-side PSK: sends `identity` during handshake, uses `resolver`
    /// to look up the shared secret.
    #[non_exhaustive]
    Client {
        /// The identity to send to the server.
        identity: Vec<u8>,
        /// Resolver for looking up shared secrets.
        resolver: Arc<dyn PskResolver>,
    },
    /// Server-side PSK: optionally sends a `hint` to help the client choose
    /// an identity, uses `resolver` to look up secrets by client identity.
    #[non_exhaustive]
    Server {
        /// Optional hint sent to the client in ServerKeyExchange.
        hint: Option<Vec<u8>>,
        /// Resolver for looking up shared secrets.
        resolver: Arc<dyn PskResolver>,
    },
}

#[cfg(feature = "aws-lc-rs")]
use crate::crypto::aws_lc_rs;

#[cfg(feature = "rust-crypto")]
use crate::crypto::rust_crypto;

/// DTLS configuration shared by all connections.
///
/// Build with [`Config::builder()`] or use [`Config::default()`].
#[derive(Clone)]
pub struct Config {
    mtu: usize,
    max_queue_rx: usize,
    max_queue_tx: usize,
    require_client_certificate: bool,
    use_server_cookie: bool,
    flight_start_rto: Duration,
    flight_retries: usize,
    handshake_timeout: Duration,
    crypto_provider: CryptoProvider,
    rng_seed: Option<u64>,
    aead_encryption_limit: u64,
    dtls12_cipher_suites: Option<Vec<Dtls12CipherSuite>>,
    dtls13_cipher_suites: Option<Vec<Dtls13CipherSuite>>,
    kx_groups: Option<Vec<NamedGroup>>,
    psk: Option<Psk>,
}

impl Config {
    /// Create a new configuration builder.
    pub fn builder() -> ConfigBuilder {
        ConfigBuilder {
            mtu: 1150,
            max_queue_rx: 30,
            max_queue_tx: 10,
            require_client_certificate: true,
            use_server_cookie: true,
            flight_start_rto: Duration::from_secs(1),
            flight_retries: 4,
            handshake_timeout: Duration::from_secs(40),
            crypto_provider: None,
            rng_seed: None,
            aead_encryption_limit: 1 << 23,
            dtls12_cipher_suites: None,
            dtls13_cipher_suites: None,
            kx_groups: None,
            psk: None,
        }
    }

    /// Max transmission unit.
    ///
    /// The largest size UDP packets we will produce.
    #[inline(always)]
    pub fn mtu(&self) -> usize {
        self.mtu
    }

    /// Max amount of incoming packets to buffer before rejecting more input.
    #[inline(always)]
    pub fn max_queue_rx(&self) -> usize {
        self.max_queue_rx
    }

    /// Max amount of outgoing packets to buffer.
    #[inline(always)]
    pub fn max_queue_tx(&self) -> usize {
        self.max_queue_tx
    }

    /// For a server, require a client certificate.
    ///
    /// This will cause the server to send a CertificateRequest message.
    /// Makes the server fail if the client does not send a certificate.
    ///
    /// Applies only to certificate-authenticated cipher suites. For RFC 4279
    /// PSK suites the client never sends a certificate, so this flag has no
    /// effect on a negotiated PSK handshake.
    #[inline(always)]
    pub fn require_client_certificate(&self) -> bool {
        self.require_client_certificate
    }

    /// Whether the server sends a cookie exchange before the handshake.
    ///
    /// When true (the default), the server requires a stateless cookie
    /// roundtrip for DoS protection: HelloVerifyRequest in DTLS 1.2,
    /// HelloRetryRequest with a cookie in DTLS 1.3.
    ///
    /// When false, the server proceeds directly to ServerHello without
    /// a cookie exchange.
    #[inline(always)]
    pub fn use_server_cookie(&self) -> bool {
        self.use_server_cookie
    }

    /// Time of first retry.
    ///
    /// Every flight restarts with this value.
    /// Doubled for every retry with a ±25% jitter.
    #[inline(always)]
    pub fn flight_start_rto(&self) -> Duration {
        self.flight_start_rto
    }

    /// Max number of retries per flight.
    #[inline(always)]
    pub fn flight_retries(&self) -> usize {
        self.flight_retries
    }

    /// Timeout for the entire handshake, regardless of flights.
    #[inline(always)]
    pub fn handshake_timeout(&self) -> Duration {
        self.handshake_timeout
    }

    /// Cryptographic provider.
    ///
    /// Provides all cryptographic operations (ciphers, key exchange, signing, etc.).
    #[inline(always)]
    pub fn crypto_provider(&self) -> &CryptoProvider {
        &self.crypto_provider
    }

    /// Optional seed for deterministic random number generation.
    ///
    /// When set, most non-cryptographic randomness (backoff jitter, TLS random bytes,
    /// AEAD nonces, cookie secrets) will be deterministic based on this seed.
    /// This is useful for testing and reproducibility.
    ///
    /// Note: Cryptographic operations (key exchange, signatures) always use
    /// secure system randomness regardless of this setting.
    #[inline(always)]
    pub fn rng_seed(&self) -> Option<u64> {
        self.rng_seed
    }

    /// Maximum number of AEAD encryptions before triggering a KeyUpdate.
    ///
    /// When the number of application-epoch ciphertext records reaches this
    /// limit, the endpoint automatically initiates a KeyUpdate to rotate keys.
    /// Defaults to 2^23 (8,388,608).
    #[inline(always)]
    pub fn aead_encryption_limit(&self) -> u64 {
        self.aead_encryption_limit
    }

    /// PSK configuration, if any.
    pub fn psk(&self) -> Option<&Psk> {
        self.psk.as_ref()
    }

    /// PSK identity for the client to send during handshake.
    pub fn psk_identity(&self) -> Option<&[u8]> {
        match &self.psk {
            Some(Psk::Client { identity, .. }) => Some(identity),
            _ => None,
        }
    }

    /// PSK identity hint for the server to send during handshake.
    pub fn psk_identity_hint(&self) -> Option<&[u8]> {
        match &self.psk {
            Some(Psk::Server { hint, .. }) => hint.as_deref(),
            _ => None,
        }
    }

    /// PSK resolver for looking up shared secrets by identity.
    pub fn psk_resolver(&self) -> Option<&dyn PskResolver> {
        match &self.psk {
            Some(Psk::Client { resolver, .. } | Psk::Server { resolver, .. }) => {
                Some(resolver.as_ref())
            }
            None => None,
        }
    }

    /// Allowed DTLS 1.2 cipher suites, filtered by the config's allow-list.
    ///
    /// Returns all provider-supported DTLS 1.2 cipher suites when no filter
    /// is set. When a filter is set via the builder's `dtls12_cipher_suites`
    /// method, only suites in both the provider and the filter are returned.
    ///
    /// PSK cipher suites are excluded when no [`PskResolver`] is configured,
    /// preventing a certificate-mode endpoint from negotiating a PSK suite
    /// and inadvertently skipping certificate authentication.
    pub fn dtls12_cipher_suites(
        &self,
    ) -> impl Iterator<Item = &'static dyn SupportedDtls12CipherSuite> + '_ {
        let filter = self.dtls12_cipher_suites.as_ref();
        let has_psk = self.psk.is_some();
        self.crypto_provider
            .supported_cipher_suites()
            .filter(move |cs| match filter {
                Some(list) => list.contains(&cs.suite()),
                None => true,
            })
            .filter(move |cs| has_psk || !cs.suite().is_psk())
    }

    /// Allowed DTLS 1.3 cipher suites, filtered by the config's allow-list.
    ///
    /// Returns all provider DTLS 1.3 cipher suites when no filter is set.
    /// When a filter is set via the builder's `dtls13_cipher_suites` method,
    /// only suites in both the provider and the filter are returned.
    pub fn dtls13_cipher_suites(
        &self,
    ) -> impl Iterator<Item = &'static dyn SupportedDtls13CipherSuite> + '_ {
        let filter = self.dtls13_cipher_suites.as_ref();
        self.crypto_provider
            .dtls13_cipher_suites
            .iter()
            .copied()
            .filter(move |cs| match filter {
                Some(list) => list.contains(&cs.suite()),
                None => true,
            })
    }

    /// Allowed key exchange groups, filtered by the config's allow-list.
    ///
    /// Returns all provider-supported key exchange groups when no filter
    /// is set. When a filter is set via the builder's `kx_groups` method,
    /// only groups in both the provider and the filter are returned.
    pub fn kx_groups(&self) -> impl Iterator<Item = &'static dyn SupportedKxGroup> + '_ {
        let filter = self.kx_groups.as_ref();
        self.crypto_provider
            .supported_kx_groups()
            .filter(move |kx| match filter {
                Some(list) => list.contains(&kx.name()),
                None => true,
            })
    }
}

/// Builder for [`Config`]. See each setter for defaults.
pub struct ConfigBuilder {
    mtu: usize,
    max_queue_rx: usize,
    max_queue_tx: usize,
    require_client_certificate: bool,
    use_server_cookie: bool,
    flight_start_rto: Duration,
    flight_retries: usize,
    handshake_timeout: Duration,
    crypto_provider: Option<CryptoProvider>,
    rng_seed: Option<u64>,
    aead_encryption_limit: u64,
    dtls12_cipher_suites: Option<Vec<Dtls12CipherSuite>>,
    dtls13_cipher_suites: Option<Vec<Dtls13CipherSuite>>,
    kx_groups: Option<Vec<NamedGroup>>,
    psk: Option<Psk>,
}

impl ConfigBuilder {
    /// Set the max transmission unit (MTU).
    ///
    /// The largest size UDP packets we will produce.
    /// Defaults to 1150.
    pub fn mtu(mut self, mtu: usize) -> Self {
        self.mtu = mtu;
        self
    }

    /// Set the max amount of incoming packets to buffer before rejecting more input.
    ///
    /// Defaults to 30.
    pub fn max_queue_rx(mut self, max_queue_rx: usize) -> Self {
        self.max_queue_rx = max_queue_rx;
        self
    }

    /// Set the max amount of outgoing packets to buffer.
    ///
    /// Defaults to 10.
    pub fn max_queue_tx(mut self, max_queue_tx: usize) -> Self {
        self.max_queue_tx = max_queue_tx;
        self
    }

    /// Set whether to require a client certificate (for servers).
    ///
    /// This will cause the server to send a CertificateRequest message.
    /// Makes the server fail if the client does not send a certificate.
    /// Defaults to true.
    ///
    /// Applies only to certificate-authenticated cipher suites. For RFC 4279
    /// PSK suites the client never sends a certificate, so this flag has no
    /// effect on a negotiated PSK handshake; no opt-out is required when
    /// combining this builder with [`with_psk_server`](Self::with_psk_server).
    pub fn require_client_certificate(mut self, require: bool) -> Self {
        self.require_client_certificate = require;
        self
    }

    /// Set whether the server sends a cookie exchange before the handshake.
    ///
    /// When true (the default), the server requires a stateless cookie
    /// roundtrip for DoS protection: HelloVerifyRequest in DTLS 1.2,
    /// HelloRetryRequest with a cookie in DTLS 1.3.
    ///
    /// When false, the server proceeds directly to ServerHello without
    /// a cookie exchange.
    pub fn use_server_cookie(mut self, use_cookie: bool) -> Self {
        self.use_server_cookie = use_cookie;
        self
    }

    /// Set the time of first retry.
    ///
    /// Each flight starts its timer when its first packet is emitted. The base
    /// interval doubles after every retry, with independent +/-25% jitter on
    /// each interval. This is proportional even for sub-second values; the
    /// minimum jittered interval is 1 nanosecond. Backoff arithmetic saturates
    /// on overflow. Also governs DTLS 1.3 KeyUpdate flights.
    ///
    /// Defaults to 1 second. Must be nonzero and at most ten years (3650 days).
    pub fn flight_start_rto(mut self, rto: Duration) -> Self {
        self.flight_start_rto = rto;
        self
    }

    /// Set the max number of retries per flight.
    ///
    /// Excludes the initial transmission: zero disables retransmissions.
    /// Timer-driven and duplicate-triggered retransmissions share this budget.
    /// Once the handshake has completed locally, the final flight is still
    /// resent in answer to the peer's retransmitted final flight, as
    /// RFC 6347 §4.2.4 requires. Also governs DTLS 1.3 KeyUpdate flights.
    /// Defaults to 4.
    pub fn flight_retries(mut self, retries: usize) -> Self {
        self.flight_retries = retries;
        self
    }

    /// Set the timeout for the entire handshake, regardless of flights.
    ///
    /// Starts when the client emits its first ClientHello packet through
    /// [`crate::Output::Packet`], or when the server accepts its first
    /// structurally valid ClientHello fragment, even if its body is later rejected.
    /// A corrected ClientHello shares the same deadline for this peer.
    /// Construction and idle time consume no budget. Uses the logical time
    /// supplied to [`crate::Dtls::handle_timeout`].
    ///
    /// The absolute deadline survives Auto version selection, cookie exchanges,
    /// later flights, and retransmissions. It is disabled after completion and
    /// does not apply to application traffic or DTLS 1.3 KeyUpdate.
    ///
    /// Retry exhaustion may fail earlier; increasing this timeout does not
    /// increase [`Self::flight_retries`]. With the defaults, an unanswered flight
    /// exhausts retries after roughly 31 seconds, before the 40-second deadline.
    ///
    /// Defaults to 40 seconds. Must be nonzero and at most ten years (3650 days).
    pub fn handshake_timeout(mut self, timeout: Duration) -> Self {
        self.handshake_timeout = timeout;
        self
    }

    /// Set a custom crypto provider.
    ///
    /// If not set, the default aws-lc-rs provider will be used, if the feature
    /// flag `aws-lc-rs` is enabled.
    pub fn with_crypto_provider(mut self, provider: CryptoProvider) -> Self {
        self.crypto_provider = Some(provider);
        self
    }

    /// Set a seed for deterministic random number generation.
    ///
    /// When set, most non-cryptographic randomness (backoff jitter, TLS random bytes,
    /// AEAD nonces, cookie secrets) will be deterministic based on this seed.
    ///
    /// This is useful for testing and reproducibility.
    ///
    /// Note: Cryptographic operations (key exchange, signatures) always use
    /// secure system randomness regardless of this setting.
    pub fn dangerously_set_rng_seed(mut self, seed: u64) -> Self {
        self.rng_seed = Some(seed);
        self
    }

    /// Set the maximum number of AEAD encryptions before triggering a KeyUpdate.
    ///
    /// Defaults to 2^23 (8,388,608).
    pub fn aead_encryption_limit(mut self, limit: u64) -> Self {
        self.aead_encryption_limit = limit;
        self
    }

    /// Restrict which DTLS 1.2 cipher suites are offered and accepted.
    ///
    /// Only cipher suites present in both this list and the provider will
    /// be used. Passing an empty slice disables DTLS 1.2 (as long as
    /// DTLS 1.3 suites remain).
    ///
    /// By default all provider-supported DTLS 1.2 cipher suites are used.
    pub fn dtls12_cipher_suites(mut self, suites: &[Dtls12CipherSuite]) -> Self {
        self.dtls12_cipher_suites = Some(suites.to_vec());
        self
    }

    /// Restrict which DTLS 1.3 cipher suites are offered and accepted.
    ///
    /// Only cipher suites present in both this list and the provider will
    /// be used. Passing an empty slice disables DTLS 1.3 (as long as
    /// DTLS 1.2 suites remain).
    ///
    /// By default all provider DTLS 1.3 cipher suites are used.
    pub fn dtls13_cipher_suites(mut self, suites: &[Dtls13CipherSuite]) -> Self {
        self.dtls13_cipher_suites = Some(suites.to_vec());
        self
    }

    /// Restrict which key exchange groups are offered and accepted.
    ///
    /// Only groups present in both this list and the provider will be
    /// used. Order determines preference (first = most preferred).
    ///
    /// By default all provider-supported key exchange groups are used.
    pub fn kx_groups(mut self, groups: &[NamedGroup]) -> Self {
        self.kx_groups = Some(groups.to_vec());
        self
    }

    /// Configure PSK for a client endpoint.
    ///
    /// The `identity` is sent to the server during the handshake.
    /// The `resolver` looks up the shared secret by identity.
    pub fn with_psk_client(mut self, identity: Vec<u8>, resolver: Arc<dyn PskResolver>) -> Self {
        self.psk = Some(Psk::Client { identity, resolver });
        self
    }

    /// Configure PSK for a server endpoint.
    ///
    /// The optional `hint` is sent to the client in ServerKeyExchange.
    /// The `resolver` looks up the shared secret by client identity.
    pub fn with_psk_server(
        mut self,
        hint: Option<Vec<u8>>,
        resolver: Arc<dyn PskResolver>,
    ) -> Self {
        self.psk = Some(Psk::Server { hint, resolver });
        self
    }

    /// Build the configuration.
    ///
    /// Validates timing settings and the crypto provider before returning the
    /// configuration. Returns `Error::ConfigError` for invalid settings.
    ///
    /// The crypto provider is selected in the following priority order:
    /// 1. Explicit provider set via `with_crypto_provider()`
    /// 2. Default provider installed via `CryptoProvider::install_default()`
    /// 3. AWS-LC provider (if `aws-lc-rs` feature is enabled)
    /// 4. RustCrypto provider (if `rust-crypto` feature is enabled)
    /// 5. Panic if no provider is available
    pub fn build(self) -> Result<Config, Error> {
        if self.handshake_timeout.is_zero() || self.handshake_timeout > MAX_TIMING_DURATION {
            return Err(ConfigError::InvalidHandshakeTimeout.into());
        }
        if self.flight_start_rto.is_zero() || self.flight_start_rto > MAX_TIMING_DURATION {
            return Err(ConfigError::InvalidFlightStartRto.into());
        }

        let crypto_provider = self
            .crypto_provider
            .or_else(|| CryptoProvider::get_default().cloned());

        #[cfg(feature = "aws-lc-rs")]
        let crypto_provider = crypto_provider.or_else(|| Some(aws_lc_rs::default_provider()));

        #[cfg(feature = "rust-crypto")]
        let crypto_provider = crypto_provider.or_else(|| Some(rust_crypto::default_provider()));

        let crypto_provider = crypto_provider.expect(
            "No crypto provider available. Either set one explicitly via \
             with_crypto_provider(), install a default via CryptoProvider::install_default(), \
             or enable the 'aws-lc-rs' or 'rust-crypto' feature.",
        );

        // Always validate the crypto provider
        crypto_provider.validate()?;

        // Validate MTU: must be large enough for DTLS record + handshake headers
        if self.mtu < 64 {
            return Err(Error::ConfigError(ConfigError::MtuTooSmall {
                mtu: self.mtu as u16,
                minimum: 64,
            }));
        }

        // Validate aead_encryption_limit: must be at least 1
        if self.aead_encryption_limit == 0 {
            return Err(Error::ConfigError(ConfigError::AeadEncryptionLimitTooSmall));
        }

        // Validate cipher suite filters: at least one version must have suites.
        // Mirror Config::dtls12_cipher_suites() by dropping PSK suites when no PSK
        // is configured, so a PSK-only filter without a PSK resolver fails fast.
        let has_psk = self.psk.is_some();
        let dtls12_suites: Vec<_> = {
            let all = crypto_provider.supported_cipher_suites();
            match &self.dtls12_cipher_suites {
                Some(list) => all
                    .filter(|cs| list.contains(&cs.suite()))
                    .filter(|cs| has_psk || !cs.suite().is_psk())
                    .collect(),
                None => all.filter(|cs| has_psk || !cs.suite().is_psk()).collect(),
            }
        };
        let dtls12_count = dtls12_suites.len();
        let dtls13_count = {
            let all = crypto_provider.dtls13_cipher_suites.iter();
            match &self.dtls13_cipher_suites {
                Some(list) => all.filter(|cs| list.contains(&cs.suite())).count(),
                None => all.count(),
            }
        };
        if dtls12_count + dtls13_count == 0 {
            return Err(Error::ConfigError(
                ConfigError::NoCipherSuitesAfterFiltering,
            ));
        }

        // When PSK is configured, at least one negotiable DTLS 1.2 suite must be
        // a PSK suite. The only PSK suite we implement today is DTLS 1.2 (0xC0A8),
        // so a surviving DTLS 1.3 suite is not a fallback: Dtls::new_12_psk only
        // speaks DTLS 1.2, and under AuthMode::Psk every non-PSK suite is rejected
        // by CryptoContext::is_cipher_suite_compatible.
        if has_psk && !dtls12_suites.iter().any(|cs| cs.suite().is_psk()) {
            return Err(Error::ConfigError(
                ConfigError::PskConfiguredWithoutPskCipherSuite,
            ));
        }

        // Skip DTLS 1.2 kx-group validation only when the surviving DTLS 1.2
        // suites are exclusively PSK — those don't negotiate an ECDHE group.
        // Any cert-based DTLS 1.2 suite left in the filter still needs a
        // compatible key exchange group, even when PSK is also configured.
        let has_non_psk_dtls12 = dtls12_suites.iter().any(|cs| !cs.suite().is_psk());

        // Validate kx_groups filter: each enabled version needs compatible groups
        // (PSK-only DTLS 1.2 configs don't need key exchange groups)
        let filtered_kx = |kx: &&'static dyn SupportedKxGroup| -> bool {
            match &self.kx_groups {
                Some(list) => list.contains(&kx.name()),
                None => true,
            }
        };
        if has_non_psk_dtls12 {
            let dtls12_kx_count = crypto_provider
                .supported_kx_groups()
                .filter(&filtered_kx)
                .count();
            if dtls12_kx_count == 0 {
                return Err(Error::ConfigError(
                    ConfigError::NoDtls12KeyExchangeGroupsAfterFiltering,
                ));
            }
        }
        if dtls13_count > 0 {
            let kx_count = crypto_provider
                .supported_kx_groups()
                .filter(filtered_kx)
                .count();
            if kx_count == 0 {
                return Err(Error::ConfigError(
                    ConfigError::NoDtls13KeyExchangeGroupsAfterFiltering,
                ));
            }
        }

        Ok(Config {
            mtu: self.mtu,
            max_queue_rx: self.max_queue_rx,
            max_queue_tx: self.max_queue_tx,
            require_client_certificate: self.require_client_certificate,
            use_server_cookie: self.use_server_cookie,
            flight_start_rto: self.flight_start_rto,
            flight_retries: self.flight_retries,
            handshake_timeout: self.handshake_timeout,
            crypto_provider,
            rng_seed: self.rng_seed,
            aead_encryption_limit: self.aead_encryption_limit,
            dtls12_cipher_suites: self.dtls12_cipher_suites,
            dtls13_cipher_suites: self.dtls13_cipher_suites,
            kx_groups: self.kx_groups,
            psk: self.psk,
        })
    }
}

impl Default for Config {
    fn default() -> Self {
        Config::builder()
            .build()
            .expect("Default config should always validate")
    }
}

impl fmt::Debug for Psk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Psk::Client { identity, .. } => f
                .debug_struct("Psk::Client")
                .field("identity", &identity)
                .field("resolver", &"...")
                .finish(),
            Psk::Server { hint, .. } => f
                .debug_struct("Psk::Server")
                .field("hint", &hint)
                .field("resolver", &"...")
                .finish(),
        }
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("mtu", &self.mtu)
            .field("max_queue_rx", &self.max_queue_rx)
            .field("max_queue_tx", &self.max_queue_tx)
            .field(
                "require_client_certificate",
                &self.require_client_certificate,
            )
            .field("use_server_cookie", &self.use_server_cookie)
            .field("flight_start_rto", &self.flight_start_rto)
            .field("flight_retries", &self.flight_retries)
            .field("handshake_timeout", &self.handshake_timeout)
            .field("crypto_provider", &self.crypto_provider)
            .field("rng_seed", &self.rng_seed)
            .field("aead_encryption_limit", &self.aead_encryption_limit)
            .field("dtls12_cipher_suites", &self.dtls12_cipher_suites)
            .field("dtls13_cipher_suites", &self.dtls13_cipher_suites)
            .field("kx_groups", &self.kx_groups)
            .field("psk", &self.psk)
            .finish()
    }
}

impl fmt::Debug for ConfigBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConfigBuilder")
            .field("mtu", &self.mtu)
            .field("max_queue_rx", &self.max_queue_rx)
            .field("max_queue_tx", &self.max_queue_tx)
            .field(
                "require_client_certificate",
                &self.require_client_certificate,
            )
            .field("use_server_cookie", &self.use_server_cookie)
            .field("flight_start_rto", &self.flight_start_rto)
            .field("flight_retries", &self.flight_retries)
            .field("handshake_timeout", &self.handshake_timeout)
            .field("crypto_provider", &self.crypto_provider)
            .field("rng_seed", &self.rng_seed)
            .field("aead_encryption_limit", &self.aead_encryption_limit)
            .field("dtls12_cipher_suites", &self.dtls12_cipher_suites)
            .field("dtls13_cipher_suites", &self.dtls13_cipher_suites)
            .field("kx_groups", &self.kx_groups)
            .field("psk", &self.psk)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timing_defaults() {
        let config = Config::default();
        assert_eq!(config.handshake_timeout(), Duration::from_secs(40));
        assert_eq!(config.flight_start_rto(), Duration::from_secs(1));
        assert_eq!(config.flight_retries(), 4);
    }

    #[test]
    fn timing_duration_bounds() {
        for duration in [
            Duration::ZERO,
            MAX_TIMING_DURATION + Duration::from_nanos(1),
            Duration::MAX,
        ] {
            let error = Config::builder()
                .handshake_timeout(duration)
                .build()
                .expect_err("unsupported handshake timeout");
            assert_eq!(
                error,
                Error::ConfigError(ConfigError::InvalidHandshakeTimeout)
            );
            assert_eq!(
                error.to_string(),
                "config error: handshake_timeout must be nonzero and at most 3650 days"
            );
            let error = Config::builder()
                .flight_start_rto(duration)
                .build()
                .expect_err("unsupported initial RTO");
            assert_eq!(
                error,
                Error::ConfigError(ConfigError::InvalidFlightStartRto)
            );
            assert_eq!(
                error.to_string(),
                "config error: flight_start_rto must be nonzero and at most 3650 days"
            );
        }
        for duration in [Duration::from_nanos(1), MAX_TIMING_DURATION] {
            for retries in [0, usize::MAX] {
                let config = Config::builder()
                    .handshake_timeout(duration)
                    .flight_start_rto(duration)
                    .flight_retries(retries)
                    .build()
                    .expect("supported timing boundaries");
                assert_eq!(config.handshake_timeout(), duration);
                assert_eq!(config.flight_start_rto(), duration);
                assert_eq!(config.flight_retries(), retries);
            }
        }
    }

    #[test]
    fn rejects_zero_mtu() {
        match Config::builder().mtu(0).build() {
            Err(Error::ConfigError(ConfigError::MtuTooSmall { mtu, minimum })) => {
                assert_eq!(mtu, 0);
                assert_eq!(minimum, 64);
            }
            Err(other) => panic!("expected ConfigError, got: {other:?}"),
            Ok(_) => panic!("expected error for MTU=0"),
        }
    }

    #[test]
    fn rejects_small_mtu() {
        match Config::builder().mtu(32).build() {
            Err(Error::ConfigError(ConfigError::MtuTooSmall { mtu, minimum })) => {
                assert_eq!(mtu, 32);
                assert_eq!(minimum, 64);
            }
            Err(other) => panic!("expected ConfigError, got: {other:?}"),
            Ok(_) => panic!("expected error for MTU=32"),
        }
    }

    #[test]
    fn accepts_minimum_mtu() {
        Config::builder()
            .mtu(64)
            .build()
            .expect("MTU 64 should be accepted");
    }

    #[test]
    fn rejects_zero_aead_limit() {
        match Config::builder().aead_encryption_limit(0).build() {
            Err(Error::ConfigError(ConfigError::AeadEncryptionLimitTooSmall)) => {}
            Err(other) => panic!("expected ConfigError, got: {other:?}"),
            Ok(_) => panic!("expected error for aead_encryption_limit=0"),
        }
    }

    #[test]
    fn accepts_minimum_aead_limit() {
        Config::builder()
            .aead_encryption_limit(1)
            .build()
            .expect("aead_encryption_limit 1 should be accepted");
    }

    #[test]
    fn filter_dtls12_cipher_suite() {
        let config = Config::builder()
            .dtls12_cipher_suites(&[Dtls12CipherSuite::ECDHE_ECDSA_AES128_GCM_SHA256])
            .build()
            .expect("should accept single DTLS 1.2 suite");
        let suites: Vec<_> = config.dtls12_cipher_suites().map(|cs| cs.suite()).collect();
        assert_eq!(suites, &[Dtls12CipherSuite::ECDHE_ECDSA_AES128_GCM_SHA256]);
    }

    #[test]
    fn filter_dtls13_cipher_suite() {
        let config = Config::builder()
            .dtls13_cipher_suites(&[Dtls13CipherSuite::AES_256_GCM_SHA384])
            .build()
            .expect("should accept single DTLS 1.3 suite");
        let suites: Vec<_> = config.dtls13_cipher_suites().map(|cs| cs.suite()).collect();
        assert_eq!(suites, &[Dtls13CipherSuite::AES_256_GCM_SHA384]);
    }

    #[test]
    fn filter_kx_groups() {
        let config = Config::builder()
            .kx_groups(&[NamedGroup::Secp256r1])
            .build()
            .expect("should accept single kx group");
        let groups: Vec<_> = config.kx_groups().map(|g| g.name()).collect();
        assert_eq!(groups, &[NamedGroup::Secp256r1]);
    }

    #[test]
    fn empty_dtls12_filter_disables_version() {
        let config = Config::builder()
            .dtls12_cipher_suites(&[])
            .build()
            .expect("should accept empty DTLS 1.2 when 1.3 has suites");
        assert_eq!(config.dtls12_cipher_suites().count(), 0);
        assert!(config.dtls13_cipher_suites().count() > 0);
    }

    #[test]
    fn empty_dtls13_filter_disables_version() {
        let config = Config::builder()
            .dtls13_cipher_suites(&[])
            .build()
            .expect("should accept empty DTLS 1.3 when 1.2 has suites");
        assert!(config.dtls12_cipher_suites().count() > 0);
        assert_eq!(config.dtls13_cipher_suites().count(), 0);
    }

    #[test]
    fn both_empty_filters_rejected() {
        match Config::builder()
            .dtls12_cipher_suites(&[])
            .dtls13_cipher_suites(&[])
            .build()
        {
            Err(Error::ConfigError(ConfigError::NoCipherSuitesAfterFiltering)) => {}
            Err(other) => panic!("expected ConfigError, got: {other:?}"),
            Ok(_) => panic!("expected error when both versions are empty"),
        }
    }

    #[test]
    fn empty_kx_groups_filter_rejected() {
        match Config::builder().kx_groups(&[]).build() {
            Err(Error::ConfigError(
                ConfigError::NoDtls12KeyExchangeGroupsAfterFiltering
                | ConfigError::NoDtls13KeyExchangeGroupsAfterFiltering,
            )) => {}
            Err(other) => panic!("expected ConfigError, got: {other:?}"),
            Ok(_) => panic!("expected error for empty kx groups"),
        }
    }

    #[test]
    fn x25519_only_accepted_for_dtls12() {
        // X25519 is supported for DTLS 1.2 and should be accepted.
        let config = Config::builder()
            .dtls13_cipher_suites(&[])
            .kx_groups(&[NamedGroup::X25519])
            .build()
            .expect("X25519-only should be accepted for DTLS 1.2");
        let groups: Vec<_> = config.kx_groups().map(|g| g.name()).collect();
        assert_eq!(groups, &[NamedGroup::X25519]);
    }

    #[test]
    fn x25519_only_accepted_for_dtls13_only() {
        // X25519-only is fine when DTLS 1.2 is disabled.
        let config = Config::builder()
            .dtls12_cipher_suites(&[])
            .kx_groups(&[NamedGroup::X25519])
            .build()
            .expect("X25519-only should be accepted for DTLS 1.3-only config");
        let groups: Vec<_> = config.kx_groups().map(|g| g.name()).collect();
        assert_eq!(groups, &[NamedGroup::X25519]);
    }

    #[test]
    fn kx_groups_match_provider_when_unfiltered() {
        let config = Config::default();
        let from_config: Vec<_> = config.kx_groups().map(|g| g.name()).collect();
        let from_provider: Vec<_> = config
            .crypto_provider()
            .supported_kx_groups()
            .map(|g| g.name())
            .collect();
        assert_eq!(from_config, from_provider);
    }

    #[test]
    fn no_filter_returns_all() {
        let config = Config::default();
        // Default provider should have at least 2 DTLS 1.2 and 2 DTLS 1.3 suites
        // (PSK suites are excluded without a resolver, so only non-PSK count)
        assert!(config.dtls12_cipher_suites().count() >= 2);
        assert!(config.dtls13_cipher_suites().count() >= 2);
        assert!(config.kx_groups().count() >= 2);
    }

    #[test]
    fn psk_suites_excluded_without_resolver() {
        let config = Config::default();
        assert!(
            config.dtls12_cipher_suites().all(|cs| !cs.suite().is_psk()),
            "PSK suites should be excluded when no PskResolver is configured"
        );
    }

    #[test]
    fn psk_suites_included_with_resolver() {
        struct DummyResolver;
        impl PskResolver for DummyResolver {
            fn resolve(&self, _identity: &[u8]) -> Option<Vec<u8>> {
                None
            }
        }

        let config = Config::builder()
            .with_psk_server(None, Arc::new(DummyResolver))
            .build()
            .expect("config with PSK resolver should build");
        assert!(
            config.dtls12_cipher_suites().any(|cs| cs.suite().is_psk()),
            "PSK suites should be included when a PskResolver is configured"
        );
    }

    #[test]
    fn psk_config_with_only_non_psk_dtls12_filter_rejected() {
        struct DummyResolver;
        impl PskResolver for DummyResolver {
            fn resolve(&self, _identity: &[u8]) -> Option<Vec<u8>> {
                Some(b"key".to_vec())
            }
        }

        // PSK config but the user filtered DTLS 1.2 down to a cert-only suite
        // and disabled DTLS 1.3. AuthMode::Psk would reject every surviving
        // suite at runtime, so build() should fail fast here.
        let result = Config::builder()
            .with_psk_client(b"identity".to_vec(), Arc::new(DummyResolver))
            .dtls12_cipher_suites(&[Dtls12CipherSuite::ECDHE_ECDSA_AES128_GCM_SHA256])
            .dtls13_cipher_suites(&[])
            .build();
        match result {
            Err(Error::ConfigError(ConfigError::PskConfiguredWithoutPskCipherSuite)) => {}
            Err(other) => panic!("expected ConfigError, got: {other:?}"),
            Ok(_) => panic!("expected error for PSK config with only non-PSK suites"),
        }
    }

    #[test]
    fn psk_with_dtls13_but_no_psk_dtls12_suite_rejected() {
        struct DummyResolver;
        impl PskResolver for DummyResolver {
            fn resolve(&self, _identity: &[u8]) -> Option<Vec<u8>> {
                Some(b"key".to_vec())
            }
        }

        // PSK configured, DTLS 1.2 filtered to cert-only, DTLS 1.3 left enabled.
        // The surviving DTLS 1.3 suite is not a fallback for Dtls::new_12_psk,
        // so build() must reject this config instead of producing one that can
        // never complete a PSK handshake.
        let result = Config::builder()
            .with_psk_client(b"identity".to_vec(), Arc::new(DummyResolver))
            .dtls12_cipher_suites(&[Dtls12CipherSuite::ECDHE_ECDSA_AES128_GCM_SHA256])
            .build();
        match result {
            Err(Error::ConfigError(ConfigError::PskConfiguredWithoutPskCipherSuite)) => {}
            Err(other) => panic!("expected ConfigError, got: {other:?}"),
            Ok(_) => panic!(
                "expected error for PSK config with only non-PSK DTLS 1.2 suites, \
                 even when DTLS 1.3 is enabled"
            ),
        }
    }

    #[test]
    fn psk_with_cert_dtls12_and_empty_kx_groups_rejected() {
        struct DummyResolver;
        impl PskResolver for DummyResolver {
            fn resolve(&self, _identity: &[u8]) -> Option<Vec<u8>> {
                Some(b"key".to_vec())
            }
        }

        // Mixed config: PSK is set, but a cert-based DTLS 1.2 suite is also in
        // the filter alongside a PSK suite. That cert suite still needs an
        // ECDHE group, so kx_groups(&[]) must fail build — the fact that PSK
        // is also configured does not excuse the missing groups.
        let result = Config::builder()
            .with_psk_server(None, Arc::new(DummyResolver))
            .dtls12_cipher_suites(&[
                Dtls12CipherSuite::ECDHE_ECDSA_AES128_GCM_SHA256,
                Dtls12CipherSuite::PSK_AES128_CCM_8,
            ])
            .dtls13_cipher_suites(&[])
            .kx_groups(&[])
            .build();
        match result {
            Err(Error::ConfigError(ConfigError::NoDtls12KeyExchangeGroupsAfterFiltering)) => {}
            Err(other) => panic!("expected ConfigError, got: {other:?}"),
            Ok(_) => panic!(
                "expected error when a cert-based DTLS 1.2 suite is enabled \
                 without any kx groups, even alongside PSK"
            ),
        }
    }

    #[test]
    fn psk_client_with_empty_kx_groups_builds() {
        struct DummyResolver;
        impl PskResolver for DummyResolver {
            fn resolve(&self, _identity: &[u8]) -> Option<Vec<u8>> {
                Some(b"key".to_vec())
            }
        }

        // PSK suites don't need ECDHE groups. A truly PSK-only endpoint (with
        // the DTLS 1.2 filter narrowed to PSK suites and DTLS 1.3 disabled)
        // should be able to opt out of kx_groups entirely.
        Config::builder()
            .with_psk_client(b"identity".to_vec(), Arc::new(DummyResolver))
            .dtls12_cipher_suites(&[Dtls12CipherSuite::PSK_AES128_CCM_8])
            .dtls13_cipher_suites(&[])
            .kx_groups(&[])
            .build()
            .expect("PSK-only client with empty kx_groups should build");
    }

    #[test]
    fn filter_with_explicit_provider() {
        #[cfg(feature = "aws-lc-rs")]
        {
            let config = Config::builder()
                .with_crypto_provider(aws_lc_rs::default_provider())
                .dtls12_cipher_suites(&[Dtls12CipherSuite::ECDHE_ECDSA_AES256_GCM_SHA384])
                .dtls13_cipher_suites(&[Dtls13CipherSuite::AES_128_GCM_SHA256])
                .kx_groups(&[NamedGroup::X25519, NamedGroup::Secp256r1])
                .build()
                .expect("should accept filtered config with explicit provider");
            let suites12: Vec<_> = config.dtls12_cipher_suites().map(|cs| cs.suite()).collect();
            assert_eq!(
                suites12,
                &[Dtls12CipherSuite::ECDHE_ECDSA_AES256_GCM_SHA384]
            );
            let suites13: Vec<_> = config.dtls13_cipher_suites().map(|cs| cs.suite()).collect();
            assert_eq!(suites13, &[Dtls13CipherSuite::AES_128_GCM_SHA256]);
            let groups: Vec<_> = config.kx_groups().map(|g| g.name()).collect();
            assert_eq!(groups, &[NamedGroup::X25519, NamedGroup::Secp256r1]);
        }
    }
}
