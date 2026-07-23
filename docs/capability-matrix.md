# Cronet safe-binding capability matrix

This matrix is audited against the pinned Chromium
`components/cronet/native/cronet.idl`, `cronet_c.h`, and
`bidirectional_stream_c.h`. “Covered” means the production capability has an
owned safe-Rust representation or is an implementation detail of that
representation. It does not mean exposing native allocation and destruction
functions to safe callers.

## Native delivery and linking

| Capability | Implementation | Verification |
| --- | --- | --- |
| Source-generated sys bindings | `cronet-sys` runs bindgen against `cronet-src`'s pinned `cronet_c.h` and `bidirectional_stream_c.h` | Workspace checks and every source build |
| Shared linking | Default mode builds and links the versioned shared Cronet target from source | Source-linked native E2E on every target |
| Static linking | Additive `static` feature builds and links the complete archive from source | Source-linked static native E2E on every target |
| Portable static archive | The build folds the GN thin archive, Chromium Rust rlibs, CXX bridge, libc++, and libc++abi into one regular archive; the final Cargo artifact supplies Rust allocator lang items, while GN-derived system libraries/frameworks remain explicit | Static source link and E2E; archive-symbol regression tests |
| Source delivery | `cronet-src::Build` prefers an explicit or vendored tree and otherwise materializes the pinned, filtered source cache; it never downloads a native library | Source-selection unit tests and six-target CI |
| OpenHarmony source build | The same builder discovers a caller-selected Native SDK and supports ARMv7, ARM64, and x86-64 without assuming a DevEco installation path | Shared/static build matrix for all three targets; application-level ARM64 QEMU E2E |

## Engine and configuration

| Upstream capability | Safe/Tokio API | Verification |
| --- | --- | --- |
| Engine start and strict-result policy | `EngineBuilder::build`; native abort-on-error is always disabled and converted to `Result` | Unit tests; every native E2E creates an engine |
| User-Agent and Accept-Language | `user_agent`, `accept_language`; per-request headers override them | `e2e_local::request_api_covers_*` |
| QUIC and QUIC hints | `enable_quic`, `QuicHint`, `quic_hint` | Native builder E2E; strict public HTTP/3 CI E2E |
| HTTP/2 | `enable_http2` | Full-duplex public HTTP/2 E2E |
| Brotli | `enable_brotli` | Locally served Brotli response is decoded E2E |
| Disabled, memory, disk-no-HTTP, and disk cache modes | `CacheMode`, `storage_path`, request `disable_cache` | `e2e_local::memory_and_disk_cache_*` |
| Public-key pins and local-anchor bypass | `PublicKeyPins`, `public_key_pins`, `bypass_pinning_for_local_trust_anchors` | Validation plus real nonmatching-chain rejection E2E |
| Android network-thread priority | `network_thread_priority` | Builder coverage; upstream says not to set it on non-Android platforms |
| Experimental options | `experimental_options`; Rust rejects malformed or non-object JSON before it reaches native startup | Valid native engine-start E2E plus malformed-input crash regression |
| NetLog start, stop, and flush | `start_net_log`, async `stop_net_log` | Non-empty NetLog file asserted E2E |
| Shutdown | async, idempotent `Engine::shutdown` | Active cancellation, repeated shutdown, and post-shutdown rejection E2E |
| Version and default User-Agent | `version`, `default_user_agent` | Native E2E |
| Engine request-finished listener add/remove | one native engine listener feeds `subscribe_finished`; receiver drop unsubscribes and native removal is synchronized with shutdown | Annotated engine-listener event and shutdown E2E |
| `GetStreamEngine` | internal to `open_bidirectional` | HTTP/2 and QUIC bidirectional E2E |

## URL requests

| Upstream capability | Safe/Tokio API | Verification |
| --- | --- | --- |
| Init and start | `Engine::start` returns `PendingRequest`; `Engine::send` awaits headers | Local E2E and `tokio::select!` branch |
| Native default GET/POST method | Method remains unset unless `method` is called | GET-without-body and POST-with-body E2E |
| Method, headers, priority, idempotency, cache, direct-executor declaration | `RequestBuilder` methods and enums | Native request E2E and validation tests |
| Opaque annotations | Safe owned strings via `annotation` | Per-request result and engine broadcast E2E |
| Redirect metadata and follow/cancel | `RedirectInfo`, `follow_redirects`, typed `Error::Redirect` | Automatic and rejected redirects E2E |
| Asynchronous redirect decision | `redirect_handler` returns a Tokio future and `RedirectAction` | Delayed async decision E2E; handler task failure becomes an error |
| Response headers and all `UrlResponseInfo` fields | owned `ResponseInfo` and `Header` values | Status/text/headers/URL chain/cache/protocol E2E |
| Read and backpressure | bounded `ResponseBody`: Tokio `AsyncRead` plus `Stream<Result<Bytes>>` | Both traits, small native buffers, bounded channel E2E |
| Cancel | `PendingRequest`, `RequestHandle`, `StreamingResponse`, and `ResponseBody` cancellation; drop also cancels | Before-headers, after-headers, drop, and shutdown E2E |
| `IsDone` | `is_done` on pending/handle/response/body | E2E |
| `GetStatus` and every status enum value | async `request_status` / `RequestHandle::status`, non-exhaustive `RequestStatus` | In-flight slow request E2E; enum mapping unit coverage |
| Success, failure, and canceled terminal callbacks | typed `Result`, `NetworkError`, `FinishedReason` | Success, response-limit failure, connection failure, and cancellation E2E |
| Request-finished metrics and listener arguments | `RequestFinishedInfo`, all timing/byte fields, response and network error | Local E2E and engine broadcast |
| Buffered convenience response | async `Engine::execute` | Used throughout E2E |

## Uploads

| Upstream capability | Safe/Tokio API | Verification |
| --- | --- | --- |
| Known-length data | `body(Bytes)` and `body_stream(_, Some(length))` | Static and Tokio duplex E2E |
| Chunked/unknown-length data | `body_stream(_, None)` | Chunked upload E2E |
| Rewind | `rewindable_body_stream(AsyncRead + AsyncSeek, length)` | Body-preserving 307 redirect E2E |
| Read/rewind success and errors | Tokio I/O mapped to exactly one native sink completion | Success E2E and declared-length/unit failure paths |
| Cancellation while a read/rewind is pending | native close cancels even permanently-pending Tokio I/O and still completes the sink contract | Dedicated pending-read and pending-rewind E2E tests |
| Close | native close is internal; Rust source is released after terminal callback and pending completions | Cancellation/shutdown E2E |

## Bidirectional HTTP/2 and QUIC streams

| Upstream capability | Safe/Tokio API | Verification |
| --- | --- | --- |
| Create/start/destroy and priority/method/headers/EOS | `BidirectionalRequestBuilder`, `Engine::open_bidirectional`, RAII | Public HTTP/2 success and local failure E2E |
| Response headers and negotiated protocol | `BidirectionalResponseHeaders` | HTTP/2 and strict QUIC E2E |
| Read callback | bounded `AsyncRead` and `Stream<Result<Bytes>>` | HTTP/2 response-body E2E |
| Write callback and bounded in-flight buffers | `AsyncWrite`, `write_capacity` | HTTP/2 POST echo E2E |
| Auto-flush, explicit flush, delayed headers | builder flags plus Tokio `flush` | HTTP/2 flush; strict QUIC delayed-header E2E |
| Half-close/end-of-stream | Tokio `shutdown` or builder `end_of_stream` | POST half-close and read-only request E2E |
| Trailers | async `trailers` | Real HTTP/2 trailing-header E2E |
| Success/failure/cancel/is-done | `finished`, typed errors, `cancel`, `is_done`, drop cancellation | Public success and local connection-failure E2E |

## Internalized ABI support

`Buffer`, `BufferCallback`, `Runnable`, `Executor`, `UploadDataSink`, callback
objects, status listeners, request-finished listeners, generated structs, and
their `Create`/`Destroy`/getter/setter functions are deliberately internal.
They are fully generated in `cronet-sys` from the pinned headers, while the
safe crate owns their lifetimes and copies all callback-borrowed data before
returning to Cronet. There is no public raw pointer or public `unsafe` function.

The native `Cronet_UrlRequest_IsDone` and `bidirectional_stream_is_done`
symbols are generated in `cronet-sys`, but the safe `is_done` methods use
terminal state set by the corresponding native success/failure/cancel
callbacks. Polling a native object after its RAII cleanup could otherwise race
destruction; the safe methods expose the same state without extending a raw
pointer's lifetime.

The exported-symbol audit found no missing production capability: every
upstream operation is either exercised by the safe layer or belongs to one of
the internal implementation interfaces above.

The sole excluded symbol is
`Cronet_Engine_SetMockCertVerifierForTesting(void *net::CertVerifier)`. It is a
testing-only C++ ownership transfer disguised as `void*`; no safe Rust value can
satisfy that contract. Exposing it as safe would invalidate the binding's
safety claim. It remains available in generated `cronet-sys` for explicitly
unsafe native-test code.

## E2E policy

- `cargo xtask audit-e2e` derives the public function set from the safe crate
  and compares it with `tests/e2e-coverage.tsv`. A new public function without
  a concrete test symbol, a removed function with a stale mapping, or a mapping
  to a missing test source fails CI. The current manifest covers 93 functions.
- `crates/cronet/tests/support/portable_e2e.rs` is the single runtime-portable
  suite compiled into desktop integration tests and the Android, iOS, and
  OpenHarmony application runners. It covers success, typed
  callback/transport failures, every request cancellation facade, drop
  cancellation, repeated/concurrent cancellation, concurrent idempotent
  shutdown, and post-shutdown rejection.
- `native-tests` runs deterministic loopback protocol tests with the real
  source-built Cronet library. Every target runs it once in default shared mode
  and once with the `static` feature.
- `network-tests` additionally runs a real TLS HTTP/2 full-duplex exchange and
  an HTTP/3/QUIC exchange. Networks may block UDP; setting
  `CRONET_E2E_REQUIRE_QUIC=1` turns QUIC fallback into a failure.
- Every desktop source-build CI target runs the deterministic suite. Linux
  x86-64 also runs a protocol gate with `CRONET_E2E_REQUIRE_QUIC=1`.
- Android and iOS runners compile the same portable source rather than a
  reduced mobile smoke test. Android's static runner additionally exercises
  the `JNI_OnLoad` JavaVM bridge.
- OpenHarmony runs that same portable source through a minimal HAP with the
  Internet permission in both shared and static modes. ARM64 is runtime-tested
  on QEMU; ARMv7 and x86-64 remain build-verified until matching runtime images
  are available. The OHOS static overlay leaves process-level libc symbols to
  libc, avoiding interposition when the archive is embedded in a Rust shared
  object.

Each native test target is a separate OS process. A callback crossing a freed
allocation, double destruction, native abort, signal, or deadlock therefore
fails the job instead of being hidden by Rust panic handling. The cancellation
and shutdown scenario repeats the high-risk ownership graph to make callback
race regressions reproducible.
