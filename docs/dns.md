# Rust-native DNS integration

The `tokio-cronet` crate enables its `dns` feature by default. The feature uses
Hickory 0.25, which supports the workspace's Rust 1.85 minimum version, and
executes DNS I/O directly on Tokio.

## Architecture boundary

There are two deliberately separate resolution paths:

```text
application lookup ── tokio_cronet::dns::DnsResolver ── Hickory ── configured DNS

Cronet request ── Chromium net::HostResolver ── Chromium sockets
               └─ Chromium TLS/QUIC ── bundled BoringSSL
```

`DnsResolver` is useful for explicit record queries, service discovery,
prewarming its own cache, health diagnostics, and application policy. The
upstream Cronet C API does not accept resolved socket addresses or a custom
resolver implementation. A Hickory result is therefore not injected into
`Engine`, and its cache is not Chromium's host cache.

Cronet does not use OpenSSL in this repository. HTTPS, HTTP/2, and QUIC remain
implemented by Chromium and its bundled BoringSSL. The public native API also
has no TLS-provider interface, so a rustls dependency in the safe binding could
not replace that implementation.

## Configuration

Use the platform DNS configuration when it is available:

```rust,no_run
use tokio_cronet::dns::DnsResolver;

let resolver = DnsResolver::from_system()?;
# Ok::<(), tokio_cronet::dns::DnsError>(())
```

Containers, mobile applications, and controlled deployments can supply
portable upstream addresses instead:

```rust,no_run
use tokio_cronet::dns::{DnsResolver, ResolverOpts};

let resolver = DnsResolver::from_name_servers(
    ["192.0.2.53:53".parse().unwrap()],
    ResolverOpts::default(),
)?;
# Ok::<(), tokio_cronet::dns::DnsError>(())
```

Each address supplied to `from_name_servers` is configured for UDP plus TCP
fallback. `from_config` exposes Hickory's complete resolver configuration for
search domains, IP strategy, retries, cache limits, hosts-file policy, TTL
bounds, server ordering, and any Hickory transport features compiled by an
application.

`lookup_ip` and `reverse_lookup` provide the common typed paths.
`lookup(name, record_type)` returns Hickory's owned `Lookup`, including record
names, TTLs, validity deadline, intermediate records, and typed `RData`. This
generic method covers all record types supported by the pinned Hickory
version, including service-discovery and mail records.

Resolver clones share the same cache and connection pool. Call `clear_cache`
when application policy requires a forced refresh. Normal cache expiration
follows the response TTL and configured minimum/maximum TTL rules.

To omit this independent resolver and its dependency graph:

```toml
tokio-cronet = { version = "0.1", default-features = false }
```

This feature selection does not change how Cronet resolves request hostnames or
performs TLS.
