# GmSSL and nginx-gmssl interoperability gate

`run.sh` builds a self-contained Linux image and verifies the public
`GmSslClient` against real `nginx-gmssl` listeners:

| Port | Protocol | Cipher suite |
| --- | --- | --- |
| 8443 | TLS 1.2 | `TLS_ECDHE_SM4_CBC_SM3` |
| 8444 | TLS 1.3 | `TLS_SM4_GCM_SM3` |
| 8445 | TLCP | `ECC_SM4_CBC_SM3` |

Run it from any workspace directory with Docker available:

```sh
tests/gmssl-e2e/run.sh
```

The image pins GmSSL v3.2.0 and `nginx-gmssl` revision
`7c70da7686ac4991b78da122d458682734721918`. GmSSL conditionally changes public
TLS structure layouts under `ENABLE_*` macros, but its installed headers do not
record the build configuration. The image therefore builds the library,
nginx, and this crate's C shim with the same explicit ABI flags.

## Pinned-backend compatibility patch

The test applies `nginx-gmssl-e2e.patch` to the pinned backend revision. It is
kept in the repository so the validation environment and its deviation from
upstream are auditable. The patch:

- allocates `TLS_CTX` outside the expiring nginx configuration-cycle pool and
  releases it during SSL cleanup;
- keeps lazy-loaded certificate, key, password, and CA paths in the nginx cycle
  pool rather than the temporary parser pool;
- removes the TLCP ECDHE cipher from the server list because GmSSL 3.2.0's
  public TLCP supported-cipher table accepts only the ECC suites.

Without the lifetime fixes, the first lazy handshake dereferences expired
configuration memory. Without the cipher adjustment, GmSSL rejects the TLCP
server context before negotiation. These changes affect only the pinned E2E
backend; the Rust client does not rely on a patched GmSSL library.
