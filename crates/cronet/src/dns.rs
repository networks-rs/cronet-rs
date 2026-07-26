//! Tokio-native application-side DNS resolution.
//!
//! [`DnsResolver`] is backed by Hickory DNS and is useful for explicit
//! lookups, cache prewarming, diagnostics, and application policy. It is
//! deliberately independent from [`crate::Engine`]: Chromium's network stack
//! continues to perform the DNS lookup used by a Cronet request. Resolving a
//! name here does not inject an address into Cronet.

use std::{
    error, fmt,
    net::{IpAddr, SocketAddr},
};

use hickory_resolver::{ResolveError, TokioResolver, name_server::TokioConnectionProvider};

pub use hickory_resolver::{
    IntoName, Name,
    config::{
        LookupIpStrategy, NameServerConfig, NameServerConfigGroup, ResolveHosts, ResolverConfig,
        ResolverOpts, ServerOrderingStrategy,
    },
    lookup::{Lookup, ReverseLookup},
    lookup_ip::LookupIp,
    proto::{
        rr::{RData, Record, RecordType},
        xfer::Protocol,
    },
};

/// Result type returned by the Tokio-native DNS API.
pub type DnsResult<T> = std::result::Result<T, DnsError>;

/// A DNS configuration or query failure.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum DnsError {
    /// A resolver cannot send queries without an upstream name server.
    NoNameServers,
    /// Hickory could not load the system configuration or resolve a query.
    Resolve(ResolveError),
}

impl DnsError {
    /// Returns whether the upstream reported that the queried name does not
    /// exist.
    #[must_use]
    pub fn is_nx_domain(&self) -> bool {
        match self {
            Self::NoNameServers => false,
            Self::Resolve(error) => error.is_nx_domain(),
        }
    }

    /// Returns whether the query completed without a record of the requested
    /// type.
    #[must_use]
    pub fn is_no_records_found(&self) -> bool {
        match self {
            Self::NoNameServers => false,
            Self::Resolve(error) => error.is_no_records_found(),
        }
    }

    /// Returns the underlying Hickory error when this was a resolver failure.
    #[must_use]
    pub fn resolve_error(&self) -> Option<&ResolveError> {
        match self {
            Self::NoNameServers => None,
            Self::Resolve(error) => Some(error),
        }
    }
}

impl fmt::Display for DnsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoNameServers => formatter.write_str("at least one DNS name server is required"),
            Self::Resolve(error) => write!(formatter, "DNS resolution failed: {error}"),
        }
    }
}

impl error::Error for DnsError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            Self::NoNameServers => None,
            Self::Resolve(error) => Some(error),
        }
    }
}

impl From<ResolveError> for DnsError {
    fn from(error: ResolveError) -> Self {
        Self::Resolve(error)
    }
}

/// A cloneable Tokio DNS resolver backed by Hickory.
///
/// The resolver owns an in-process cache. Clones share that cache and the
/// underlying connection pool, so it is inexpensive to keep one resolver in
/// application state and clone it into Tokio tasks.
#[derive(Clone)]
pub struct DnsResolver {
    inner: TokioResolver,
}

impl DnsResolver {
    /// Creates a resolver from the host's DNS configuration.
    ///
    /// Unix-like targets read `resolv.conf` and Windows reads the registry.
    /// Loading configuration is fallible so containers and mobile platforms
    /// can explicitly fall back to [`Self::from_name_servers`].
    pub fn from_system() -> DnsResult<Self> {
        let inner = TokioResolver::builder_tokio()?.build();
        Ok(Self { inner })
    }

    /// Creates a resolver from explicit Hickory configuration and options.
    ///
    /// This is the full configuration escape hatch for search domains,
    /// lookup strategy, TTL limits, retry policy, hosts-file behavior, and
    /// custom transports compiled into Hickory.
    #[must_use]
    pub fn from_config(config: ResolverConfig, options: ResolverOpts) -> Self {
        let mut builder =
            TokioResolver::builder_with_config(config, TokioConnectionProvider::default());
        *builder.options_mut() = options;
        Self {
            inner: builder.build(),
        }
    }

    /// Creates a resolver using cleartext DNS over UDP with TCP fallback.
    ///
    /// Each supplied socket address is registered for both UDP and TCP. An
    /// empty iterator is rejected instead of creating a resolver that can
    /// only time out.
    pub fn from_name_servers(
        name_servers: impl IntoIterator<Item = SocketAddr>,
        options: ResolverOpts,
    ) -> DnsResult<Self> {
        let mut group = NameServerConfigGroup::new();
        for socket_addr in name_servers {
            group.push(NameServerConfig::new(socket_addr, Protocol::Udp));
            group.push(NameServerConfig::new(socket_addr, Protocol::Tcp));
        }
        if group.is_empty() {
            return Err(DnsError::NoNameServers);
        }

        Ok(Self::from_config(
            ResolverConfig::from_parts(None, Vec::new(), group),
            options,
        ))
    }

    /// Returns the effective resolver configuration.
    #[must_use]
    pub fn config(&self) -> &ResolverConfig {
        self.inner.config()
    }

    /// Returns the effective retry, cache, lookup, and validation options.
    #[must_use]
    pub fn options(&self) -> &ResolverOpts {
        self.inner.options()
    }

    /// Removes positive and negative entries from this resolver's cache.
    pub fn clear_cache(&self) {
        self.inner.clear_cache();
    }

    /// Resolves any record type supported by Hickory.
    ///
    /// The returned records retain their owner names, TTLs, intermediate
    /// records, and typed [`RData`]. This method is the complete DNS query
    /// surface; the convenience methods below cover address and reverse
    /// lookups.
    pub async fn lookup(&self, name: impl IntoName, record_type: RecordType) -> DnsResult<Lookup> {
        self.inner
            .lookup(name, record_type)
            .await
            .map_err(Into::into)
    }

    /// Resolves IPv4 and/or IPv6 addresses according to
    /// [`ResolverOpts::ip_strategy`].
    pub async fn lookup_ip(&self, host: impl IntoName) -> DnsResult<LookupIp> {
        self.inner.lookup_ip(host).await.map_err(Into::into)
    }

    /// Resolves the PTR records for an IP address.
    pub async fn reverse_lookup(&self, address: IpAddr) -> DnsResult<ReverseLookup> {
        self.inner.reverse_lookup(address).await.map_err(Into::into)
    }
}

impl fmt::Debug for DnsResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DnsResolver")
            .field("config", self.config())
            .field("options", self.options())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_name_server_list_is_rejected() {
        let error = DnsResolver::from_name_servers([], ResolverOpts::default()).unwrap_err();
        assert!(matches!(error, DnsError::NoNameServers));
        assert!(error.resolve_error().is_none());
        assert!(!error.is_nx_domain());
        assert!(!error.is_no_records_found());
    }

    #[test]
    fn explicit_configuration_is_observable() {
        let mut options = ResolverOpts::default();
        options.attempts = 1;
        options.cache_size = 64;
        let resolver =
            DnsResolver::from_name_servers(["127.0.0.1:5353".parse().unwrap()], options).unwrap();

        assert_eq!(resolver.config().name_servers().len(), 2);
        assert_eq!(resolver.options().attempts, 1);
        assert_eq!(resolver.options().cache_size, 64);
        resolver.clear_cache();
    }
}
