# Safe binding E2E contract

“Complete E2E coverage” in this repository means every public safe function
has a concrete runtime scenario and every native ownership boundary is tested
in its meaningful terminal states. It does not claim that a finite suite can
enumerate every server, proxy, carrier, TLS middlebox, or kernel failure.

The machine-readable mapping is `tests/e2e-coverage.tsv`; run:

```text
cargo xtask audit-e2e
```

The audit derives the safe crate's public function set and fails on missing or
stale rows. It also rejects scenario names that do not occur in test sources.

## Required scenario dimensions

| Boundary | Required runtime scenarios |
| --- | --- |
| Engine | full builder configuration, version/UA, cache modes, NetLog, normal shutdown, repeated and concurrent shutdown, active-operation shutdown, post-shutdown rejection |
| URL request | buffered and streamed success, all control/status facades, response limit, redirect follow/reject/async decision, callback task panic, connection failure, truncated response |
| Upload | bytes, known length, unknown/chunked length, rewind success, short source, injected `AsyncRead` error, injected `AsyncSeek` error, cancellation during pending read and rewind |
| Response body | bounded backpressure, `AsyncRead`, `Stream`, `next_chunk`, metrics, explicit cancel, handle cancel, response/body drop |
| Bidirectional stream | HTTP/2 and HTTP/3 open, headers, trailers, `AsyncRead`, `AsyncWrite`, `Stream`, `next_chunk`, flush, half-close, terminal success/failure/cancel, active drop |
| Concurrency/lifetime | concurrent requests, repeated cancellation from cloned handles, callback completion during drop, concurrent shutdown, engine drop with active work |
| Native delivery | shared and static linkage execute the same suite; mobile/OpenHarmony application runners compile the same portable scenario source |

`crates/cronet/tests/support/portable_e2e.rs` is included directly by desktop,
Android/iOS, and OHOS
runners so platform suites cannot silently drift. Desktop-only cache/NetLog
tests and public-network HTTP/2/HTTP/3 tests extend it where loopback or mobile
sandboxing cannot provide the protocol.

Native integration tests run as OS test processes. Segmentation faults,
double-free aborts, uncaught native exceptions, and deadlocks are job failures;
they are not converted into successful Rust assertions. The portable stress
scenario repeats cancellation/drop/shutdown ownership races before returning.
