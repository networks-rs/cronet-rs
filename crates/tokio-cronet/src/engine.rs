use std::{
    collections::HashMap,
    ffi::{CString, c_void},
    path::{Path, PathBuf},
    ptr,
    sync::{
        Arc, Condvar, Mutex, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use tokio::{
    runtime::Handle,
    sync::{Notify, OnceCell, broadcast},
};
use tokio_cronet_sys as sys;

use crate::{
    BidirectionalRequest, BidirectionalRequestBuilder, BidirectionalStream, Error, PendingRequest,
    Request, RequestBuilder, RequestFinishedInfo, Response, Result, StreamingResponse,
    bidirectional::open as open_bidirectional,
    error::check,
    executor::Executor,
    request::{execute_request, start_request},
    types::{copy_c_string, copy_finished_info, to_cstring, validate_string},
};

/// Native HTTP cache configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CacheMode {
    #[default]
    Disabled,
    InMemory {
        max_size: i64,
    },
    DiskNoHttp {
        max_size: i64,
    },
    Disk {
        max_size: i64,
    },
}

/// An alternative QUIC endpoint advertised to Cronet before DNS discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuicHint {
    host: String,
    port: u16,
    alternate_port: u16,
}

impl QuicHint {
    pub fn new(host: impl Into<String>, port: u16, alternate_port: u16) -> Result<Self> {
        let host = host.into();
        validate_string(&host, "QUIC hint host")?;
        if host.is_empty() || port == 0 || alternate_port == 0 {
            return Err(Error::InvalidConfiguration(
                "QUIC hint host and ports must be non-empty/non-zero",
            ));
        }
        Ok(Self {
            host,
            port,
            alternate_port,
        })
    }
}

/// Public-key pins for one host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKeyPins {
    host: String,
    pins_sha256: Vec<String>,
    include_subdomains: bool,
    expiration_date: i64,
}

impl PublicKeyPins {
    /// Creates a pin set. Each pin must use Cronet's `sha256/<base64>` format.
    pub fn new(
        host: impl Into<String>,
        pins_sha256: impl IntoIterator<Item = impl Into<String>>,
        include_subdomains: bool,
        expiration_date_unix_millis: i64,
    ) -> Result<Self> {
        let host = host.into();
        validate_string(&host, "public-key pin host")?;
        let pins_sha256 = pins_sha256.into_iter().map(Into::into).collect::<Vec<_>>();
        if host.is_empty() || pins_sha256.is_empty() {
            return Err(Error::InvalidConfiguration(
                "public-key pins require a host and at least one pin",
            ));
        }
        for pin in &pins_sha256 {
            validate_string(pin, "public-key pin")?;
            if !pin.starts_with("sha256/") {
                return Err(Error::InvalidConfiguration(
                    "public-key pins must use sha256/<base64> format",
                ));
            }
        }
        Ok(Self {
            host,
            pins_sha256,
            include_subdomains,
            expiration_date: expiration_date_unix_millis,
        })
    }
}

/// Configures and starts a Cronet engine.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct EngineBuilder {
    user_agent: Option<String>,
    accept_language: Option<String>,
    storage_path: Option<PathBuf>,
    enable_quic: bool,
    enable_http2: bool,
    enable_brotli: bool,
    cache: CacheMode,
    bypass_pinning_for_local_anchors: bool,
    network_thread_priority: Option<i32>,
    experimental_options: Option<String>,
    quic_hints: Vec<QuicHint>,
    public_key_pins: Vec<PublicKeyPins>,
    #[cfg(feature = "nqe")]
    pub(crate) enable_network_quality_estimator: bool,
}

impl Default for EngineBuilder {
    fn default() -> Self {
        Self {
            user_agent: None,
            accept_language: None,
            storage_path: None,
            enable_quic: true,
            enable_http2: true,
            enable_brotli: true,
            cache: CacheMode::Disabled,
            bypass_pinning_for_local_anchors: true,
            network_thread_priority: None,
            experimental_options: None,
            quic_hints: Vec::new(),
            public_key_pins: Vec::new(),
            #[cfg(feature = "nqe")]
            enable_network_quality_estimator: false,
        }
    }
}

impl EngineBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn user_agent(mut self, value: impl Into<String>) -> Self {
        self.user_agent = Some(value.into());
        self
    }

    #[must_use]
    pub fn accept_language(mut self, value: impl Into<String>) -> Self {
        self.accept_language = Some(value.into());
        self
    }

    #[must_use]
    pub fn storage_path(mut self, value: impl Into<PathBuf>) -> Self {
        self.storage_path = Some(value.into());
        self
    }

    #[must_use]
    pub const fn enable_quic(mut self, value: bool) -> Self {
        self.enable_quic = value;
        self
    }

    #[must_use]
    pub const fn enable_http2(mut self, value: bool) -> Self {
        self.enable_http2 = value;
        self
    }

    #[must_use]
    pub const fn enable_brotli(mut self, value: bool) -> Self {
        self.enable_brotli = value;
        self
    }

    #[must_use]
    pub const fn cache_mode(mut self, value: CacheMode) -> Self {
        self.cache = value;
        self
    }

    #[must_use]
    pub const fn bypass_pinning_for_local_trust_anchors(mut self, value: bool) -> Self {
        self.bypass_pinning_for_local_anchors = value;
        self
    }

    #[must_use]
    pub const fn network_thread_priority(mut self, value: i32) -> Self {
        self.network_thread_priority = Some(value);
        self
    }

    #[must_use]
    pub fn experimental_options(mut self, json: impl Into<String>) -> Self {
        self.experimental_options = Some(json.into());
        self
    }

    #[must_use]
    pub fn quic_hint(mut self, hint: QuicHint) -> Self {
        self.quic_hints.push(hint);
        self
    }

    #[must_use]
    pub fn public_key_pins(mut self, pins: PublicKeyPins) -> Self {
        self.public_key_pins.push(pins);
        self
    }

    /// Starts a new engine on the current Tokio runtime.
    #[allow(clippy::too_many_lines)]
    pub fn build(self) -> Result<Engine> {
        validate_cache_size(self.cache)?;
        validate_experimental_options(self.experimental_options.as_deref())?;
        if self.network_thread_priority.is_some() && !cfg!(target_os = "android") {
            return Err(Error::InvalidConfiguration(
                "network thread priority is supported only on Android",
            ));
        }
        if matches!(
            self.cache,
            CacheMode::Disk { .. } | CacheMode::DiskNoHttp { .. }
        ) && !self.storage_path.as_deref().is_some_and(Path::is_dir)
        {
            return Err(Error::StoragePathMissing);
        }

        let user_agent = optional_cstring(self.user_agent.as_deref(), "user agent")?;
        let accept_language = optional_cstring(self.accept_language.as_deref(), "accept language")?;
        let storage_path = self
            .storage_path
            .as_deref()
            .map(path_to_cstring)
            .transpose()?;
        let experimental_options =
            optional_cstring(self.experimental_options.as_deref(), "experimental options")?;
        let executor = Executor::new()?;

        // SAFETY: native constructors take no borrowed state.
        let raw = unsafe { sys::Cronet_Engine_Create() };
        if raw.is_null() {
            return Err(Error::AllocationFailed("engine"));
        }
        // SAFETY: native constructor takes no borrowed state.
        let params = unsafe { sys::Cronet_EngineParams_Create() };
        if params.is_null() {
            // SAFETY: engine has not been started.
            unsafe { sys::Cronet_Engine_Destroy(raw) };
            return Err(Error::AllocationFailed("engine parameters"));
        }

        // SAFETY: params is exclusively owned and setters copy scalar/string data.
        unsafe {
            // A safe binding must return errors instead of asking Cronet to abort.
            sys::Cronet_EngineParams_enable_check_result_set(params, false);
            sys::Cronet_EngineParams_enable_quic_set(params, self.enable_quic);
            sys::Cronet_EngineParams_enable_http2_set(params, self.enable_http2);
            sys::Cronet_EngineParams_enable_brotli_set(params, self.enable_brotli);
            sys::Cronet_EngineParams_enable_public_key_pinning_bypass_for_local_trust_anchors_set(
                params,
                self.bypass_pinning_for_local_anchors,
            );
            let (mode, max_size) = cache_parts(self.cache);
            sys::Cronet_EngineParams_http_cache_mode_set(params, mode);
            sys::Cronet_EngineParams_http_cache_max_size_set(params, max_size);
            if let Some(value) = &user_agent {
                sys::Cronet_EngineParams_user_agent_set(params, value.as_ptr());
            }
            if let Some(value) = &accept_language {
                sys::Cronet_EngineParams_accept_language_set(params, value.as_ptr());
            }
            if let Some(value) = &storage_path {
                sys::Cronet_EngineParams_storage_path_set(params, value.as_ptr());
            }
            if let Some(value) = &experimental_options {
                sys::Cronet_EngineParams_experimental_options_set(params, value.as_ptr());
            }
            if let Some(value) = self.network_thread_priority {
                sys::Cronet_EngineParams_network_thread_priority_set(params, f64::from(value));
            }
        }

        let configure_result =
            configure_endpoint_hints(params, &self.quic_hints, &self.public_key_pins);
        if let Err(error) = configure_result {
            // SAFETY: neither object was handed to a successfully started engine.
            unsafe {
                sys::Cronet_EngineParams_Destroy(params);
                sys::Cronet_Engine_Destroy(raw);
            }
            return Err(error);
        }

        // SAFETY: engine and params are live for this call.
        let result = unsafe { sys::Cronet_RS_Engine_StartWithParams(raw, params) };
        // SAFETY: Cronet copies parameters during StartWithParams.
        unsafe { sys::Cronet_EngineParams_Destroy(params) };
        if let Err(error) = check(result) {
            // SAFETY: start failed and no request can reference the engine.
            unsafe { sys::Cronet_Engine_Destroy(raw) };
            return Err(error);
        }

        let (finished_context, finished_listener) = match attach_finished_listener(raw, &executor) {
            Ok(attached) => attached,
            Err(error) => {
                // SAFETY: no request has been admitted and the engine started.
                let _ = check(unsafe { sys::Cronet_Engine_Shutdown(raw) });
                // SAFETY: shutdown above leaves no native references.
                unsafe { sys::Cronet_Engine_Destroy(raw) };
                return Err(error);
            }
        };
        #[cfg(feature = "nqe")]
        let nqe = self.enable_network_quality_estimator.then(|| {
            crate::nqe::NqeState::start(finished_context.events.subscribe(), executor.handle())
        });
        Ok(Engine {
            inner: Arc::new(EngineInner {
                native: Mutex::new(Some(NativeEngine {
                    raw,
                    executor: Some(executor),
                    finished_listener: Some(finished_listener),
                })),
                closing: AtomicBool::new(false),
                active: AtomicUsize::new(0),
                idle: Notify::new(),
                controls: Mutex::new(Vec::new()),
                finished_context,
                shutdown_result: OnceCell::new(),
                #[cfg(feature = "nqe")]
                nqe,
            }),
        })
    }
}

fn attach_finished_listener(
    engine: sys::Cronet_EnginePtr,
    executor: &Executor,
) -> Result<(Arc<EngineFinishedContext>, EngineFinishedListener)> {
    let (events, _) = broadcast::channel(256);
    let context = Arc::new(EngineFinishedContext::new(events));
    let listener = EngineFinishedListener::new(&context)?;
    // SAFETY: all three objects remain owned by NativeEngine until removal.
    unsafe {
        sys::Cronet_Engine_AddRequestFinishedListener(engine, listener.raw, executor.as_ptr());
    }
    Ok((context, listener))
}

/// Handle that restores Cronet's default network selection.
#[cfg(feature = "network-binding")]
pub const UNBIND_NETWORK_HANDLE: i64 = -1;

/// A cloneable, `Send + Sync` Cronet engine attached to a Tokio runtime.
#[derive(Clone)]
pub struct Engine {
    pub(crate) inner: Arc<EngineInner>,
}

impl Engine {
    #[must_use]
    pub fn builder() -> EngineBuilder {
        EngineBuilder::new()
    }

    pub fn request(&self, url: impl Into<String>) -> Result<RequestBuilder> {
        RequestBuilder::new(url)
    }

    pub fn bidirectional_request(
        &self,
        url: impl Into<String>,
    ) -> Result<BidirectionalRequestBuilder> {
        BidirectionalRequestBuilder::new(url)
    }

    /// Opens a Tokio full-duplex HTTP/2 or QUIC stream.
    pub async fn open_bidirectional(
        &self,
        request: BidirectionalRequest,
    ) -> Result<BidirectionalStream> {
        open_bidirectional(self, request).await
    }

    /// Starts a request and resolves when response headers are available.
    pub async fn send(&self, request: Request) -> Result<StreamingResponse> {
        self.start(request)?.await
    }

    /// Starts a request immediately and returns a Tokio future plus control handle.
    pub fn start(&self, request: Request) -> Result<PendingRequest> {
        start_request(self, request)
    }

    /// Sends a request and buffers its body while preserving asynchronous I/O.
    pub async fn execute(&self, request: Request) -> Result<Response> {
        execute_request(self, request).await
    }

    /// Subscribes to final metrics for every request sent by this safe engine.
    #[must_use]
    pub fn subscribe_finished(&self) -> broadcast::Receiver<RequestFinishedInfo> {
        self.inner.finished_context.events.subscribe()
    }

    pub fn version(&self) -> Result<String> {
        self.with_native(|native| {
            // SAFETY: native is locked and remains live for this call.
            unsafe { copy_c_string(sys::Cronet_Engine_GetVersionString(native.raw)) }
        })
    }

    pub fn default_user_agent(&self) -> Result<String> {
        self.with_native(|native| {
            // SAFETY: native is locked and remains live for this call.
            unsafe { copy_c_string(sys::Cronet_Engine_GetDefaultUserAgent(native.raw)) }
        })
    }

    pub fn start_net_log(&self, path: &Path, log_all: bool) -> Result<bool> {
        let path = path_to_cstring(path)?;
        self.with_native(|native| {
            // SAFETY: engine and C string remain live for this call.
            unsafe { sys::Cronet_Engine_StartNetLogToFile(native.raw, path.as_ptr(), log_all) }
        })
    }

    /// Stops and flushes `NetLog` without blocking a Tokio worker.
    pub async fn stop_net_log(&self) -> Result<()> {
        let operation = self.inner.begin_operation()?;
        tokio::task::spawn_blocking(move || {
            // SAFETY: operation keeps the engine active until this call finishes.
            unsafe { sys::Cronet_Engine_StopNetLog(operation.raw()) };
            drop(operation);
        })
        .await
        .map_err(|error| Error::TokioTask(error.to_string()))
    }

    /// Cancels active requests, waits for terminal callbacks, and shuts down.
    pub async fn shutdown(&self) -> Result<()> {
        self.inner.shutdown().await
    }

    /// Binds subsequent WebSocket connections to `network_handle`.
    ///
    /// Pass [`UNBIND_NETWORK_HANDLE`] to use the default network. Upstream
    /// `UrlRequest` has no bind API, so HTTP requests ignore this handle.
    #[cfg(feature = "network-binding")]
    pub fn bind_to_network(&self, network_handle: i64) -> Result<()> {
        self.with_native(|native| {
            // SAFETY: engine is live and the wrapper table does not retain `raw`.
            unsafe { sys::Cronet_RS_Engine_BindToNetwork(native.raw, network_handle) };
        })
    }

    /// Returns the handle last passed to [`Self::bind_to_network`].
    #[cfg(feature = "network-binding")]
    pub fn bound_network(&self) -> Result<i64> {
        self.with_native(|native| {
            // SAFETY: engine is live.
            unsafe { sys::Cronet_RS_Engine_GetBoundNetwork(native.raw) }
        })
    }

    fn with_native<T>(&self, function: impl FnOnce(&NativeEngine) -> T) -> Result<T> {
        if self.inner.closing.load(Ordering::Acquire) {
            return Err(Error::EngineShutdown);
        }
        let guard = self
            .inner
            .native
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.inner.closing.load(Ordering::Acquire) {
            return Err(Error::EngineShutdown);
        }
        guard.as_ref().map(function).ok_or(Error::EngineShutdown)
    }
}

pub(crate) trait RequestCanceler: Send + Sync {
    fn cancel(&self);
}

pub(crate) struct EngineOperation {
    engine: Arc<EngineInner>,
    raw: usize,
    executor: usize,
    handle: Handle,
}

impl EngineOperation {
    pub(crate) fn raw(&self) -> sys::Cronet_EnginePtr {
        self.raw as sys::Cronet_EnginePtr
    }
    pub(crate) fn executor(&self) -> sys::Cronet_ExecutorPtr {
        self.executor as sys::Cronet_ExecutorPtr
    }
    pub(crate) fn handle(&self) -> &Handle {
        &self.handle
    }
}

impl Drop for EngineOperation {
    fn drop(&mut self) {
        if self.engine.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.engine.idle.notify_waiters();
        }
    }
}

pub(crate) struct EngineInner {
    native: Mutex<Option<NativeEngine>>,
    closing: AtomicBool,
    active: AtomicUsize,
    idle: Notify,
    controls: Mutex<Vec<Weak<dyn RequestCanceler>>>,
    finished_context: Arc<EngineFinishedContext>,
    shutdown_result: OnceCell<Result<()>>,
    #[cfg(feature = "nqe")]
    pub(crate) nqe: Option<Arc<crate::nqe::NqeState>>,
}

impl EngineInner {
    pub(crate) fn begin_finished_request(&self, annotations: &[(usize, Arc<u8>, Option<String>)]) {
        self.finished_context.begin_request(annotations);
    }

    pub(crate) fn abort_finished_request(&self, annotations: &[(usize, Arc<u8>, Option<String>)]) {
        self.finished_context.abort_request(annotations);
    }

    pub(crate) fn begin_operation(self: &Arc<Self>) -> Result<EngineOperation> {
        if self.closing.load(Ordering::Acquire) {
            return Err(Error::EngineShutdown);
        }
        let guard = self
            .native
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.closing.load(Ordering::Acquire) {
            return Err(Error::EngineShutdown);
        }
        let native = guard.as_ref().ok_or(Error::EngineShutdown)?;
        self.active.fetch_add(1, Ordering::AcqRel);
        Ok(EngineOperation {
            engine: self.clone(),
            raw: native.raw as usize,
            executor: native.executor().as_ptr() as usize,
            handle: native.executor().handle().clone(),
        })
    }

    pub(crate) fn register(&self, control: &Arc<dyn RequestCanceler>) -> bool {
        let mut controls = self
            .controls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        controls.retain(|control| control.strong_count() != 0);
        controls.push(Arc::downgrade(control));
        let closing = self.closing.load(Ordering::Acquire);
        drop(controls);
        if closing {
            control.cancel();
        }
        closing
    }

    async fn shutdown(self: &Arc<Self>) -> Result<()> {
        // Serialize the admission boundary: an operation either increments the
        // active count first or observes closing while holding this same lock.
        {
            let _native = self
                .native
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.closing.store(true, Ordering::Release);
        }
        let controls = {
            let controls = self
                .controls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            controls
                .iter()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>()
        };
        for control in controls {
            control.cancel();
        }

        self.shutdown_result
            .get_or_init(|| async {
                while self.active.load(Ordering::Acquire) != 0 {
                    let notified = self.idle.notified();
                    if self.active.load(Ordering::Acquire) != 0 {
                        notified.await;
                    }
                }
                self.finished_context.wait_idle().await;
                let native = self
                    .native
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                let Some(native) = native else {
                    return Ok(());
                };
                tokio::task::spawn_blocking(move || native.shutdown())
                    .await
                    .map_err(|error| Error::TokioTask(error.to_string()))?
            })
            .await
            .clone()
    }
}

impl Drop for EngineInner {
    fn drop(&mut self) {
        let native = self
            .native
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(native) = native {
            let finished_context = self.finished_context.clone();
            let _ = std::thread::Builder::new()
                .name("cronet-shutdown".to_owned())
                .spawn(move || {
                    finished_context.wait_idle_blocking();
                    let _ = native.shutdown();
                });
        }
    }
}

type RegisteredAnnotation = (Arc<u8>, Option<String>);

struct EngineFinishedContext {
    events: broadcast::Sender<RequestFinishedInfo>,
    annotations: Mutex<HashMap<usize, RegisteredAnnotation>>,
    pending: Mutex<usize>,
    idle: Notify,
    idle_blocking: Condvar,
}

impl EngineFinishedContext {
    fn new(events: broadcast::Sender<RequestFinishedInfo>) -> Self {
        Self {
            events,
            annotations: Mutex::new(HashMap::new()),
            pending: Mutex::new(0),
            idle: Notify::new(),
            idle_blocking: Condvar::new(),
        }
    }

    fn begin_request(&self, annotations: &[(usize, Arc<u8>, Option<String>)]) {
        let mut registered = self
            .annotations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (pointer, token, value) in annotations {
            registered.insert(*pointer, (token.clone(), value.clone()));
        }
        drop(registered);
        *self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
    }

    fn abort_request(&self, annotations: &[(usize, Arc<u8>, Option<String>)]) {
        let mut registered = self
            .annotations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (pointer, _, _) in annotations {
            registered.remove(pointer);
        }
        drop(registered);
        self.finish_one();
    }

    unsafe fn on_finished(
        &self,
        request_info: sys::Cronet_RequestFinishedInfoPtr,
        response_info: sys::Cronet_UrlResponseInfoPtr,
        error: sys::Cronet_ErrorPtr,
    ) {
        let count = if request_info.is_null() {
            0
        } else {
            // SAFETY: the native callback owns request_info for this call.
            unsafe { sys::Cronet_RequestFinishedInfo_annotations_size(request_info) }
        };
        let mut raw_annotations = Vec::with_capacity(count as usize);
        for index in 0..count {
            // SAFETY: index is bounded by the native annotation count.
            raw_annotations.push(unsafe {
                sys::Cronet_RequestFinishedInfo_annotations_at(request_info, index)
            });
        }
        let mut safe_annotations = Vec::new();
        let mut tracked_request = false;
        {
            let mut registered = self
                .annotations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for pointer in &raw_annotations {
                if let Some((_, value)) = registered.remove(&(*pointer as usize)) {
                    if let Some(value) = value {
                        safe_annotations.push((*pointer, value));
                    } else {
                        tracked_request = true;
                    }
                }
            }
        }
        if !tracked_request {
            return;
        }
        // SAFETY: every callback-owned object is copied before this returns.
        let info =
            unsafe { copy_finished_info(request_info, response_info, error, &safe_annotations) };
        let _ = self.events.send(info);
        self.finish_one();
    }

    fn finish_one(&self) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *pending == 0 {
            return;
        }
        *pending -= 1;
        if *pending == 0 {
            self.idle.notify_waiters();
            self.idle_blocking.notify_all();
        }
    }

    async fn wait_idle(&self) {
        loop {
            let notified = self.idle.notified();
            if *self
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                == 0
            {
                return;
            }
            notified.await;
        }
    }

    fn wait_idle_blocking(&self) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *pending != 0 {
            pending = self
                .idle_blocking
                .wait(pending)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

struct EngineFinishedListener {
    raw: sys::Cronet_RequestFinishedInfoListenerPtr,
    context: *const EngineFinishedContext,
}

// SAFETY: the listener is removed only after all tracked callbacks have begun;
// callback state is Arc-backed and internally synchronized.
unsafe impl Send for EngineFinishedListener {}

impl EngineFinishedListener {
    fn new(context: &Arc<EngineFinishedContext>) -> Result<Self> {
        // SAFETY: callback has the generated C ABI.
        let raw = unsafe {
            sys::Cronet_RequestFinishedInfoListener_CreateWith(Some(on_engine_request_finished))
        };
        if raw.is_null() {
            return Err(Error::AllocationFailed("engine request-finished listener"));
        }
        let context = Arc::into_raw(context.clone());
        // SAFETY: the raw Arc is retained through listener destruction.
        unsafe {
            sys::Cronet_RequestFinishedInfoListener_SetClientContext(
                raw,
                context.cast_mut().cast::<c_void>(),
            );
        }
        Ok(Self { raw, context })
    }

    fn unregister(&self, engine: sys::Cronet_EnginePtr) {
        // SAFETY: engine and listener are live and no tracked callback is pending.
        unsafe { sys::Cronet_Engine_RemoveRequestFinishedListener(engine, self.raw) };
    }
}

impl Drop for EngineFinishedListener {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            // SAFETY: listener has been removed and no callback can start.
            unsafe { sys::Cronet_RequestFinishedInfoListener_Destroy(self.raw) };
            self.raw = ptr::null_mut();
        }
        if !self.context.is_null() {
            // SAFETY: reverses Arc::into_raw in new after listener removal.
            unsafe { drop(Arc::from_raw(self.context)) };
            self.context = ptr::null();
        }
    }
}

unsafe extern "C" fn on_engine_request_finished(
    listener: sys::Cronet_RequestFinishedInfoListenerPtr,
    request_info: sys::Cronet_RequestFinishedInfoPtr,
    response_info: sys::Cronet_UrlResponseInfoPtr,
    error: sys::Cronet_ErrorPtr,
) {
    // SAFETY: listener is our live object and retains this Arc raw pointer.
    let raw = unsafe { sys::Cronet_RequestFinishedInfoListener_GetClientContext(listener) }
        .cast::<EngineFinishedContext>();
    if raw.is_null() {
        return;
    }
    // SAFETY: increment before constructing a temporary Arc keeps the listener's
    // original strong reference owned by EngineFinishedListener.
    let context = unsafe {
        Arc::increment_strong_count(raw);
        Arc::from_raw(raw)
    };
    // SAFETY: callback-owned data stays live for this invocation.
    unsafe { context.on_finished(request_info, response_info, error) };
}

struct NativeEngine {
    raw: sys::Cronet_EnginePtr,
    executor: Option<Executor>,
    finished_listener: Option<EngineFinishedListener>,
}

// SAFETY: access is serialized by EngineInner; Cronet's engine API supports
// requests and shutdown from non-network threads.
unsafe impl Send for NativeEngine {}

impl NativeEngine {
    fn executor(&self) -> &Executor {
        self.executor.as_ref().expect("live engine owns executor")
    }

    fn shutdown(mut self) -> Result<()> {
        if let Some(listener) = self.finished_listener.take() {
            listener.unregister(self.raw);
            drop(listener);
        }
        // SAFETY: wrapper table is process-local and keyed by this engine pointer.
        unsafe { sys::Cronet_RS_Engine_ClearBoundNetwork(self.raw) };
        // SAFETY: EngineInner waits for all operations before shutdown.
        check(unsafe { sys::Cronet_Engine_Shutdown(self.raw) })?;
        // SAFETY: successful shutdown releases all native engine use.
        unsafe { sys::Cronet_Engine_Destroy(self.raw) };
        self.raw = ptr::null_mut();
        self.executor.take();
        Ok(())
    }
}

impl Drop for NativeEngine {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            // A failed shutdown can leave native work in flight. Leaking the
            // executor is safer than destroying its callback while Cronet uses it.
            if let Some(executor) = self.executor.take() {
                std::mem::forget(executor);
            }
        }
    }
}

fn configure_endpoint_hints(
    params: sys::Cronet_EngineParamsPtr,
    hints: &[QuicHint],
    pin_sets: &[PublicKeyPins],
) -> Result<()> {
    for hint in hints {
        // SAFETY: constructor takes no borrowed state.
        let raw = unsafe { sys::Cronet_QuicHint_Create() };
        if raw.is_null() {
            return Err(Error::AllocationFailed("QUIC hint"));
        }
        let host = CString::new(hint.host.as_bytes()).expect("QuicHint validated host");
        // SAFETY: values are copied into params before temporary destruction.
        unsafe {
            sys::Cronet_QuicHint_host_set(raw, host.as_ptr());
            sys::Cronet_QuicHint_port_set(raw, i32::from(hint.port));
            sys::Cronet_QuicHint_alternate_port_set(raw, i32::from(hint.alternate_port));
            sys::Cronet_EngineParams_quic_hints_add(params, raw);
            sys::Cronet_QuicHint_Destroy(raw);
        }
    }
    for pins in pin_sets {
        // SAFETY: constructor takes no borrowed state.
        let raw = unsafe { sys::Cronet_PublicKeyPins_Create() };
        if raw.is_null() {
            return Err(Error::AllocationFailed("public-key pins"));
        }
        let host = CString::new(pins.host.as_bytes()).expect("PublicKeyPins validated host");
        // SAFETY: values are copied into params before temporary destruction.
        unsafe {
            sys::Cronet_PublicKeyPins_host_set(raw, host.as_ptr());
            sys::Cronet_PublicKeyPins_include_subdomains_set(raw, pins.include_subdomains);
            sys::Cronet_PublicKeyPins_expiration_date_set(raw, pins.expiration_date);
            for pin in &pins.pins_sha256 {
                let pin = CString::new(pin.as_bytes()).expect("PublicKeyPins validated pin");
                sys::Cronet_PublicKeyPins_pins_sha256_add(raw, pin.as_ptr());
            }
            sys::Cronet_EngineParams_public_key_pins_add(params, raw);
            sys::Cronet_PublicKeyPins_Destroy(raw);
        }
    }
    Ok(())
}

fn optional_cstring(value: Option<&str>, field: &'static str) -> Result<Option<CString>> {
    value.map(|value| to_cstring(value, field)).transpose()
}

fn path_to_cstring(path: &Path) -> Result<CString> {
    let value = path.to_str().ok_or(Error::NonUtf8Path)?;
    to_cstring(value, "path")
}

const fn cache_parts(cache: CacheMode) -> (sys::Cronet_EngineParams_HTTP_CACHE_MODE, i64) {
    match cache {
        CacheMode::Disabled => (sys::Cronet_EngineParams_HTTP_CACHE_MODE_DISABLED, 0),
        CacheMode::InMemory { max_size } => {
            (sys::Cronet_EngineParams_HTTP_CACHE_MODE_IN_MEMORY, max_size)
        }
        CacheMode::DiskNoHttp { max_size } => (
            sys::Cronet_EngineParams_HTTP_CACHE_MODE_DISK_NO_HTTP,
            max_size,
        ),
        CacheMode::Disk { max_size } => (sys::Cronet_EngineParams_HTTP_CACHE_MODE_DISK, max_size),
    }
}

const fn cache_size(cache: CacheMode) -> Option<i64> {
    match cache {
        CacheMode::Disabled => None,
        CacheMode::InMemory { max_size }
        | CacheMode::DiskNoHttp { max_size }
        | CacheMode::Disk { max_size } => Some(max_size),
    }
}

fn validate_cache_size(cache: CacheMode) -> Result<()> {
    if cache_size(cache).is_some_and(|size| !(0..=i32::MAX.into()).contains(&size)) {
        Err(Error::InvalidConfiguration(
            "HTTP cache size must fit a non-negative native int",
        ))
    } else {
        Ok(())
    }
}

fn validate_experimental_options(options: Option<&str>) -> Result<()> {
    let Some(options) = options else {
        return Ok(());
    };
    if serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(options).is_err() {
        return Err(Error::InvalidConfiguration(
            "experimental options must be a valid JSON object",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_engine_endpoint_configuration() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Engine>();
        assert!(QuicHint::new("example.com", 443, 443).is_ok());
        assert!(PublicKeyPins::new("example.com", ["not-a-pin"], false, 0).is_err());
    }

    #[test]
    fn rejects_cache_sizes_the_native_builder_cannot_represent() {
        for max_size in [-1, i64::from(i32::MAX) + 1] {
            assert!(matches!(
                validate_cache_size(CacheMode::InMemory { max_size }),
                Err(Error::InvalidConfiguration(_))
            ));
        }
    }

    #[test]
    fn rejects_experimental_options_that_can_crash_native_startup() {
        for options in ["", "{", "[]", "null", "true"] {
            assert!(matches!(
                validate_experimental_options(Some(options)),
                Err(Error::InvalidConfiguration(_))
            ));
        }
        assert!(validate_experimental_options(Some("{}")).is_ok());
        assert!(validate_experimental_options(Some(r#"{"QUIC": {}}"#)).is_ok());
        assert!(validate_experimental_options(None).is_ok());
    }

    #[cfg(not(target_os = "android"))]
    #[tokio::test]
    async fn rejects_android_thread_priority_on_other_platforms() {
        assert!(matches!(
            Engine::builder().network_thread_priority(0).build(),
            Err(Error::InvalidConfiguration(_))
        ));
    }
}
