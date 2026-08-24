use std::{
    ffi::c_void,
    fmt,
    future::Future,
    io::{self, SeekFrom},
    pin::Pin,
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

use bytes::{Buf, Bytes};
use futures_core::Stream;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt, ReadBuf},
    runtime::Handle,
    sync::{mpsc, oneshot, watch},
};
use tokio_cronet_sys as sys;

use crate::{
    Engine, Error, Header, RedirectInfo, RequestFinishedInfo, RequestStatus, Response,
    ResponseInfo, Result,
    engine::{EngineOperation, RequestCanceler},
    error::{ResultCode, check},
    types::{
        copy_c_string, copy_finished_info, copy_network_error, copy_response_info, to_cstring,
        validate_string,
    },
};

const DEFAULT_BUFFER_SIZE: usize = 32 * 1024;
const DEFAULT_BODY_CHANNEL_CAPACITY: usize = 8;
const DEFAULT_MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// Cronet request priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Priority {
    Idle,
    Lowest,
    Low,
    #[default]
    Medium,
    Highest,
}

/// Declares whether a request can safely be retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Idempotency {
    #[default]
    Default,
    Idempotent,
    NotIdempotent,
}

/// Decision returned by an asynchronous redirect handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RedirectAction {
    Follow,
    Cancel,
}

type RedirectFuture = Pin<Box<dyn Future<Output = RedirectAction> + Send>>;
type RedirectHandler = Arc<dyn Fn(RedirectInfo) -> RedirectFuture + Send + Sync>;

enum RedirectPolicy {
    Follow,
    Cancel,
    Handler(RedirectHandler),
}

impl fmt::Debug for RedirectPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Follow => formatter.write_str("Follow"),
            Self::Cancel => formatter.write_str("Cancel"),
            Self::Handler(_) => formatter.write_str("Handler(..)"),
        }
    }
}

trait AsyncReadSeek: AsyncRead + AsyncSeek {}
impl<T: AsyncRead + AsyncSeek + ?Sized> AsyncReadSeek for T {}

enum UploadBody {
    Bytes(Bytes),
    Reader {
        reader: Pin<Box<dyn AsyncRead + Send>>,
        length: Option<u64>,
    },
    Rewindable {
        reader: Pin<Box<dyn AsyncReadSeek + Send>>,
        length: Option<u64>,
    },
}

#[cfg(feature = "gmssl_tls")]
pub(crate) struct GmSslRequestParts {
    pub(crate) url: String,
    pub(crate) method: String,
    pub(crate) headers: Vec<Header>,
    pub(crate) body: Option<Bytes>,
    pub(crate) max_response_bytes: usize,
}

/// An immutable request ready to send once.
pub struct Request {
    url: String,
    method: Option<String>,
    headers: Vec<Header>,
    body: Option<UploadBody>,
    disable_cache: bool,
    priority: Priority,
    idempotency: Idempotency,
    redirect_policy: RedirectPolicy,
    allow_direct_executor: bool,
    buffer_size: usize,
    body_channel_capacity: usize,
    max_response_bytes: usize,
    annotations: Vec<String>,
}

impl fmt::Debug for Request {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Request")
            .field("url", &self.url)
            .field("method", &self.method)
            .field("headers", &self.headers)
            .field("has_body", &self.body.is_some())
            .field("disable_cache", &self.disable_cache)
            .field("priority", &self.priority)
            .field("idempotency", &self.idempotency)
            .field("redirect_policy", &self.redirect_policy)
            .field("allow_direct_executor", &self.allow_direct_executor)
            .field("buffer_size", &self.buffer_size)
            .field("body_channel_capacity", &self.body_channel_capacity)
            .field("max_response_bytes", &self.max_response_bytes)
            .field("annotations", &self.annotations)
            .finish()
    }
}

impl Request {
    pub fn builder(url: impl Into<String>) -> Result<RequestBuilder> {
        RequestBuilder::new(url)
    }

    #[cfg(feature = "gmssl_tls")]
    pub(crate) fn into_gmssl_parts(self) -> Result<GmSslRequestParts> {
        let default_method = if self.body.is_some() { "POST" } else { "GET" };
        let method = self.method.unwrap_or_else(|| default_method.to_owned());
        let body = match self.body {
            Some(UploadBody::Bytes(body)) => Some(body),
            Some(UploadBody::Reader { .. } | UploadBody::Rewindable { .. }) => {
                return Err(crate::gmssl::GmSslError::StreamingUploadUnsupported.into());
            }
            None => None,
        };
        Ok(GmSslRequestParts {
            url: self.url,
            method,
            headers: self.headers,
            body,
            max_response_bytes: self.max_response_bytes,
        })
    }
}

/// Builder for buffered or streaming Cronet requests.
#[derive(Debug)]
pub struct RequestBuilder {
    request: Request,
}

impl RequestBuilder {
    pub fn new(url: impl Into<String>) -> Result<Self> {
        let url = url.into();
        validate_string(&url, "URL")?;
        Ok(Self {
            request: Request {
                url,
                method: None,
                headers: Vec::new(),
                body: None,
                disable_cache: false,
                priority: Priority::Medium,
                idempotency: Idempotency::Default,
                redirect_policy: RedirectPolicy::Follow,
                allow_direct_executor: false,
                buffer_size: DEFAULT_BUFFER_SIZE,
                body_channel_capacity: DEFAULT_BODY_CHANNEL_CAPACITY,
                max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
                annotations: Vec::new(),
            },
        })
    }

    pub fn method(mut self, method: impl Into<String>) -> Result<Self> {
        let method = method.into();
        validate_string(&method, "HTTP method")?;
        self.request.method = Some(method);
        Ok(self)
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Result<Self> {
        self.request.headers.push(Header::new(name, value)?);
        Ok(self)
    }

    /// Adds an opaque request annotation, returned in final metrics.
    pub fn annotation(mut self, value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_string(&value, "request annotation")?;
        self.request.annotations.push(value);
        Ok(self)
    }

    #[must_use]
    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.request.body = Some(UploadBody::Bytes(body.into()));
        self
    }

    /// Uses a Tokio `AsyncRead` as a one-shot upload source.
    #[must_use]
    pub fn body_stream<R>(mut self, reader: R, content_length: Option<u64>) -> Self
    where
        R: AsyncRead + Send + 'static,
    {
        self.request.body = Some(UploadBody::Reader {
            reader: Box::pin(reader),
            length: content_length,
        });
        self
    }

    /// Uses a seekable Tokio source that Cronet can rewind for retries.
    #[must_use]
    pub fn rewindable_body_stream<R>(mut self, reader: R, content_length: Option<u64>) -> Self
    where
        R: AsyncRead + AsyncSeek + Send + 'static,
    {
        self.request.body = Some(UploadBody::Rewindable {
            reader: Box::pin(reader),
            length: content_length,
        });
        self
    }

    #[must_use]
    pub const fn disable_cache(mut self, value: bool) -> Self {
        self.request.disable_cache = value;
        self
    }

    #[must_use]
    pub const fn priority(mut self, value: Priority) -> Self {
        self.request.priority = value;
        self
    }

    #[must_use]
    pub const fn idempotency(mut self, value: Idempotency) -> Self {
        self.request.idempotency = value;
        self
    }

    #[must_use]
    pub fn follow_redirects(mut self, value: bool) -> Self {
        self.request.redirect_policy = if value {
            RedirectPolicy::Follow
        } else {
            RedirectPolicy::Cancel
        };
        self
    }

    /// Lets a Tokio future decide each redirect without blocking Cronet.
    #[must_use]
    pub fn redirect_handler<F, Fut>(mut self, handler: F) -> Self
    where
        F: Fn(RedirectInfo) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = RedirectAction> + Send + 'static,
    {
        self.request.redirect_policy =
            RedirectPolicy::Handler(Arc::new(move |redirect| Box::pin(handler(redirect))));
        self
    }

    /// Allows Cronet's direct-executor optimization for this request.
    #[must_use]
    pub const fn allow_direct_executor(mut self, value: bool) -> Self {
        self.request.allow_direct_executor = value;
        self
    }

    #[must_use]
    pub const fn read_buffer_size(mut self, bytes: usize) -> Self {
        self.request.buffer_size = bytes;
        self
    }

    /// Sets the number of body chunks buffered between Cronet and the consumer.
    #[must_use]
    pub const fn body_channel_capacity(mut self, chunks: usize) -> Self {
        self.request.body_channel_capacity = chunks;
        self
    }

    /// Limits both streamed and buffered bodies. The default is 64 MiB.
    #[must_use]
    pub const fn max_response_bytes(mut self, bytes: usize) -> Self {
        self.request.max_response_bytes = bytes;
        self
    }

    pub fn build(self) -> Result<Request> {
        if self.request.buffer_size == 0 {
            return Err(Error::InvalidConfiguration(
                "read buffer size must be greater than zero",
            ));
        }
        if self.request.buffer_size > i32::MAX as usize {
            return Err(Error::InvalidConfiguration(
                "read buffer size does not fit Cronet's native int read length",
            ));
        }
        if self.request.body_channel_capacity == 0 {
            return Err(Error::InvalidConfiguration(
                "body channel capacity must be greater than zero",
            ));
        }
        let length = self.request.body.as_ref().and_then(upload_body_length);
        if length.is_some_and(|length| i64::try_from(length).is_err()) {
            return Err(Error::InvalidConfiguration(
                "request body does not fit Cronet's int64_t length",
            ));
        }
        Ok(self.request)
    }
}

fn upload_body_length(body: &UploadBody) -> Option<u64> {
    match body {
        UploadBody::Bytes(body) => u64::try_from(body.len()).ok(),
        UploadBody::Reader { length, .. } | UploadBody::Rewindable { length, .. } => *length,
    }
}

/// A response whose body is consumed through Tokio I/O.
pub struct StreamingResponse {
    pub info: ResponseInfo,
    pub body: ResponseBody,
}

impl StreamingResponse {
    #[must_use]
    pub fn status(&self) -> i32 {
        self.info.status_code
    }

    pub async fn request_status(&self) -> Result<RequestStatus> {
        self.body.request_status().await
    }

    pub fn cancel(&self) {
        self.body.cancel();
    }

    /// Returns a cloneable control handle for cancellation and status queries.
    #[must_use]
    pub fn handle(&self) -> RequestHandle {
        self.body.handle()
    }

    /// Returns whether Cronet has delivered a terminal callback.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.body.is_done()
    }

    #[must_use]
    pub fn into_parts(self) -> (ResponseInfo, ResponseBody) {
        (self.info, self.body)
    }
}

impl fmt::Debug for StreamingResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StreamingResponse")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

/// A bounded Cronet body stream that also implements Tokio `AsyncRead`.
pub struct ResponseBody {
    receiver: mpsc::Receiver<Result<Bytes>>,
    current: Bytes,
    control: Arc<RequestControl>,
    finished: watch::Receiver<Option<RequestFinishedInfo>>,
}

impl ResponseBody {
    pub async fn next_chunk(&mut self) -> Option<Result<Bytes>> {
        self.receiver.recv().await
    }

    pub fn cancel(&self) {
        self.control.cancel();
    }

    /// Returns a cloneable control handle which does not keep the body alive.
    #[must_use]
    pub fn handle(&self) -> RequestHandle {
        RequestHandle {
            control: self.control.clone(),
        }
    }

    /// Returns whether Cronet has delivered a terminal callback.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.control.is_done()
    }

    pub async fn request_status(&self) -> Result<RequestStatus> {
        self.control.status().await
    }

    /// Waits for metrics and the request-finished reason.
    pub async fn finished(&mut self) -> Result<RequestFinishedInfo> {
        loop {
            if let Some(info) = self.finished.borrow().clone() {
                return Ok(info);
            }
            self.finished
                .changed()
                .await
                .map_err(|_| Error::CallbackChannelClosed)?;
        }
    }
}

impl fmt::Debug for ResponseBody {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResponseBody")
            .field("is_done", &self.control.is_done())
            .finish_non_exhaustive()
    }
}

impl Stream for ResponseBody {
    type Item = Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.current.has_remaining() {
            return Poll::Ready(Some(Ok(std::mem::take(&mut self.current))));
        }
        self.receiver.poll_recv(context)
    }
}

impl AsyncRead for ResponseBody {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let starting = output.filled().len();
        loop {
            if self.current.has_remaining() {
                let count = self.current.remaining().min(output.remaining());
                output.put_slice(&self.current[..count]);
                self.current.advance(count);
                return Poll::Ready(Ok(()));
            }
            match self.receiver.poll_recv(context) {
                Poll::Ready(Some(Ok(chunk))) => self.current = chunk,
                Poll::Ready(Some(Err(error))) => {
                    return Poll::Ready(Err(io::Error::other(error.to_string())));
                }
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Pending if output.filled().len() != starting => return Poll::Ready(Ok(())),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl Drop for ResponseBody {
    fn drop(&mut self) {
        if !self.control.is_done() {
            self.control.cancel();
        }
    }
}

pub(crate) async fn execute_request(engine: &Engine, request: Request) -> Result<Response> {
    let mut response = start_request(engine, request)?.await?;
    let mut body = Vec::new();
    while let Some(chunk) = response.body.next_chunk().await {
        body.extend_from_slice(&chunk?);
    }
    let finished = response.body.finished().await?;
    Ok(Response {
        info: response.info,
        body,
        finished,
    })
}

/// A started request waiting for response headers.
///
/// Dropping this future cancels the native request. Use [`Self::handle`] to
/// query status or cancel it from another Tokio task or `select!` branch.
pub struct PendingRequest {
    headers: oneshot::Receiver<Result<ResponseInfo>>,
    body: Option<mpsc::Receiver<Result<Bytes>>>,
    control: Arc<RequestControl>,
    finished: Option<watch::Receiver<Option<RequestFinishedInfo>>>,
    completed: bool,
}

impl PendingRequest {
    #[must_use]
    pub fn handle(&self) -> RequestHandle {
        RequestHandle {
            control: self.control.clone(),
        }
    }

    pub fn cancel(&self) {
        self.control.cancel();
    }

    #[must_use]
    pub fn is_done(&self) -> bool {
        self.control.is_done()
    }

    pub async fn request_status(&self) -> Result<RequestStatus> {
        self.control.status().await
    }
}

impl fmt::Debug for PendingRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PendingRequest")
            .field("is_done", &self.is_done())
            .finish_non_exhaustive()
    }
}

impl Future for PendingRequest {
    type Output = Result<StreamingResponse>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.headers).poll(context) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(Ok(info))) => {
                self.completed = true;
                let receiver = self.body.take().expect("pending request body is available");
                let finished = self
                    .finished
                    .take()
                    .expect("pending request metrics receiver is available");
                Poll::Ready(Ok(StreamingResponse {
                    info,
                    body: ResponseBody {
                        receiver,
                        current: Bytes::new(),
                        control: self.control.clone(),
                        finished,
                    },
                }))
            }
            Poll::Ready(Ok(Err(error))) => {
                self.completed = true;
                Poll::Ready(Err(error))
            }
            Poll::Ready(Err(_)) => {
                self.control.cancel();
                self.completed = true;
                Poll::Ready(Err(Error::CallbackChannelClosed))
            }
        }
    }
}

impl Drop for PendingRequest {
    fn drop(&mut self) {
        if !self.completed && !self.control.is_done() {
            self.control.cancel();
        }
    }
}

/// A cloneable, thread-safe view of a live Cronet request.
#[derive(Clone)]
pub struct RequestHandle {
    control: Arc<RequestControl>,
}

impl RequestHandle {
    pub fn cancel(&self) {
        self.control.cancel();
    }

    #[must_use]
    pub fn is_done(&self) -> bool {
        self.control.is_done()
    }

    pub async fn status(&self) -> Result<RequestStatus> {
        self.control.status().await
    }
}

impl fmt::Debug for RequestHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RequestHandle")
            .field("is_done", &self.is_done())
            .finish()
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn start_request(engine: &Engine, request: Request) -> Result<PendingRequest> {
    let Request {
        url,
        method,
        headers,
        body,
        disable_cache,
        priority,
        idempotency,
        redirect_policy,
        allow_direct_executor,
        buffer_size,
        body_channel_capacity,
        max_response_bytes,
        annotations,
    } = request;
    let url = to_cstring(&url, "URL")?;
    let method = method
        .as_deref()
        .map(|method| to_cstring(method, "HTTP method"))
        .transpose()?;
    let operation = engine.inner.begin_operation()?;
    let handle = operation.handle().clone();
    let (headers_sender, headers_receiver) = oneshot::channel();
    let (body_sender, body_receiver) = mpsc::channel(body_channel_capacity);
    let (terminal_sender, terminal_receiver) = oneshot::channel();
    let (finished_sender, finished_receiver) = watch::channel(None);
    let request_error = Arc::new(Mutex::new(None));
    let control = Arc::new(RequestControl::default());
    let context = Arc::new(CallbackContext {
        state: Mutex::new(CallbackState::default()),
        callback_count: AtomicUsize::new(0),
        callbacks_idle: tokio::sync::Notify::new(),
        headers_sender: Mutex::new(Some(headers_sender)),
        body_sender,
        terminal_sender: Mutex::new(Some(terminal_sender)),
        finished_sender,
        redirect_policy,
        buffer_size,
        max_response_bytes,
        handle: handle.clone(),
        control: control.clone(),
        request_error: request_error.clone(),
        tracking_annotation: Annotation::tracking(),
        annotations: annotations.into_iter().map(Annotation::new).collect(),
    });
    let mut native = NativeRequest::new(&context, operation)?;
    let canceler: Arc<dyn RequestCanceler> = control.clone();
    if engine.inner.register(&canceler) {
        return Err(Error::EngineShutdown);
    }

    // SAFETY: params is exclusively owned until initialization completes.
    unsafe {
        if let Some(method) = &method {
            sys::Cronet_UrlRequestParams_http_method_set(native.params, method.as_ptr());
        }
        sys::Cronet_UrlRequestParams_disable_cache_set(native.params, disable_cache);
        sys::Cronet_UrlRequestParams_priority_set(native.params, priority_raw(priority));
        sys::Cronet_UrlRequestParams_idempotency_set(native.params, idempotency_raw(idempotency));
        sys::Cronet_UrlRequestParams_allow_direct_executor_set(
            native.params,
            allow_direct_executor,
        );
        sys::Cronet_UrlRequestParams_request_finished_listener_set(
            native.params,
            native.finished_listener,
        );
        sys::Cronet_UrlRequestParams_request_finished_executor_set(
            native.params,
            native.operation().executor(),
        );
        sys::Cronet_UrlRequestParams_annotations_add(
            native.params,
            context.tracking_annotation.pointer(),
        );
        for annotation in &context.annotations {
            sys::Cronet_UrlRequestParams_annotations_add(native.params, annotation.pointer());
        }
    }
    for header in &headers {
        native.add_header(header)?;
    }
    if let Some(body) = body {
        native.set_upload(body, request_error)?;
    }

    // SAFETY: all native objects and C strings are live; Cronet copies params.
    let result = unsafe {
        sys::Cronet_UrlRequest_InitWithParams(
            native.request,
            native.operation().raw(),
            url.as_ptr(),
            native.params,
            native.callback,
            native.operation().executor(),
        )
    };
    check(result)?;
    native.destroy_params();
    let engine_annotations = context.engine_annotations();
    engine.inner.begin_finished_request(&engine_annotations);
    // SAFETY: successful initialization makes the request startable.
    if let Err(error) = check(unsafe { sys::Cronet_UrlRequest_Start(native.request) }) {
        engine.inner.abort_finished_request(&engine_annotations);
        return Err(error);
    }
    native.started = true;
    if control.cancel_requested.load(Ordering::Acquire) {
        control.cancel();
    }

    let mut cleanup_finished = finished_receiver.clone();
    let cleanup_context = context.clone();
    handle.spawn(async move {
        let _ = terminal_receiver.await;
        while cleanup_finished.borrow().is_none() {
            if cleanup_finished.changed().await.is_err() {
                break;
            }
        }
        cleanup_context.wait_for_callbacks().await;
        drop(native);
    });

    Ok(PendingRequest {
        headers: headers_receiver,
        body: Some(body_receiver),
        control,
        finished: Some(finished_receiver),
        completed: false,
    })
}

#[derive(Default)]
pub(crate) struct RequestControl {
    raw: AtomicUsize,
    done: AtomicBool,
    cancel_requested: AtomicBool,
    gate: Mutex<()>,
}

impl RequestControl {
    fn set_raw(&self, raw: sys::Cronet_UrlRequestPtr) {
        self.raw.store(raw as usize, Ordering::Release);
    }
    fn is_done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    fn cancel(&self) {
        self.cancel_requested.store(true, Ordering::Release);
        let _guard = self
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let raw = self.raw.load(Ordering::Acquire) as sys::Cronet_UrlRequestPtr;
        if !raw.is_null() && !self.is_done() {
            // SAFETY: gate prevents concurrent native destruction.
            unsafe { sys::Cronet_UrlRequest_Cancel(raw) };
        }
    }

    fn follow_redirect(&self) -> Result<()> {
        let _guard = self
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let raw = self.raw.load(Ordering::Acquire) as sys::Cronet_UrlRequestPtr;
        if raw.is_null() || self.is_done() {
            return Err(Error::Canceled);
        }
        // SAFETY: gate keeps the initialized request live for this call.
        check(unsafe { sys::Cronet_UrlRequest_FollowRedirect(raw) })
    }

    async fn status(&self) -> Result<RequestStatus> {
        if self.is_done() {
            return Ok(RequestStatus::Invalid);
        }
        let (sender, receiver) = oneshot::channel();
        let context = Box::into_raw(Box::new(StatusContext {
            sender: Some(sender),
        }));
        // SAFETY: callback has the required ABI.
        let listener = unsafe { sys::Cronet_UrlRequestStatusListener_CreateWith(Some(on_status)) };
        if listener.is_null() {
            // SAFETY: reverses Box::into_raw above.
            unsafe { drop(Box::from_raw(context)) };
            return Err(Error::AllocationFailed("request status listener"));
        }
        // SAFETY: listener owns the boxed context until on_status.
        unsafe {
            sys::Cronet_UrlRequestStatusListener_SetClientContext(listener, context.cast());
        }
        {
            let _guard = self
                .gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let raw = self.raw.load(Ordering::Acquire) as sys::Cronet_UrlRequestPtr;
            if raw.is_null() || self.is_done() {
                // SAFETY: GetStatus was not called, so cleanup remains ours.
                unsafe {
                    sys::Cronet_UrlRequestStatusListener_Destroy(listener);
                    drop(Box::from_raw(context));
                }
                return Ok(RequestStatus::Invalid);
            }
            // SAFETY: gate keeps the request live through GetStatus registration.
            unsafe { sys::Cronet_UrlRequest_GetStatus(raw, listener) };
        }
        receiver.await.map_err(|_| Error::CallbackChannelClosed)
    }

    fn destroy_request(&self, request: sys::Cronet_UrlRequestPtr) {
        let _guard = self
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.raw.store(0, Ordering::Release);
        // SAFETY: request is either not started or has reached terminal callback.
        unsafe { sys::Cronet_UrlRequest_Destroy(request) };
    }
}

impl RequestCanceler for RequestControl {
    fn cancel(&self) {
        RequestControl::cancel(self);
    }
}

struct StatusContext {
    sender: Option<oneshot::Sender<RequestStatus>>,
}

unsafe extern "C" fn on_status(
    listener: sys::Cronet_UrlRequestStatusListenerPtr,
    status: sys::Cronet_UrlRequestStatusListener_Status,
) {
    // SAFETY: listener was created with this boxed client context.
    let raw = unsafe { sys::Cronet_UrlRequestStatusListener_GetClientContext(listener) }
        .cast::<StatusContext>();
    if !raw.is_null() {
        // SAFETY: this callback runs exactly once and reclaims the unique Box.
        let mut context = unsafe { Box::from_raw(raw) };
        if let Some(sender) = context.sender.take() {
            let _ = sender.send(RequestStatus::from_raw(status));
        }
    }
    // SAFETY: status delivery is complete and listener is no longer needed.
    unsafe { sys::Cronet_UrlRequestStatusListener_Destroy(listener) };
}

struct NativeRequest {
    request: sys::Cronet_UrlRequestPtr,
    callback: sys::Cronet_UrlRequestCallbackPtr,
    finished_listener: sys::Cronet_RequestFinishedInfoListenerPtr,
    params: sys::Cronet_UrlRequestParamsPtr,
    context: *const CallbackContext,
    control: Arc<RequestControl>,
    upload: Option<UploadProvider>,
    operation: Option<EngineOperation>,
    started: bool,
}

// SAFETY: ownership is transferred to one Tokio cleanup task. All callback
// state is synchronized and opaque native pointers are only accessed per API.
unsafe impl Send for NativeRequest {}

impl NativeRequest {
    fn new(context: &Arc<CallbackContext>, operation: EngineOperation) -> Result<Self> {
        // SAFETY: constructors take no borrowed state.
        let params = unsafe { sys::Cronet_UrlRequestParams_Create() };
        if params.is_null() {
            return Err(Error::AllocationFailed("request parameters"));
        }
        // SAFETY: callbacks have the ABI and lifetime required by Cronet.
        let callback = unsafe {
            sys::Cronet_UrlRequestCallback_CreateWith(
                Some(on_redirect_received),
                Some(on_response_started),
                Some(on_read_completed),
                Some(on_succeeded),
                Some(on_failed),
                Some(on_canceled),
            )
        };
        if callback.is_null() {
            // SAFETY: params is uniquely owned.
            unsafe { sys::Cronet_UrlRequestParams_Destroy(params) };
            return Err(Error::AllocationFailed("request callback"));
        }
        // SAFETY: callback has the required ABI.
        let finished_listener = unsafe {
            sys::Cronet_RequestFinishedInfoListener_CreateWith(Some(on_request_finished))
        };
        if finished_listener.is_null() {
            // SAFETY: objects are uniquely owned.
            unsafe {
                sys::Cronet_UrlRequestCallback_Destroy(callback);
                sys::Cronet_UrlRequestParams_Destroy(params);
            }
            return Err(Error::AllocationFailed("request-finished listener"));
        }
        let context_raw = Arc::into_raw(context.clone());
        // SAFETY: raw Arc remains live through NativeRequest drop.
        unsafe {
            sys::Cronet_UrlRequestCallback_SetClientContext(
                callback,
                context_raw.cast_mut().cast(),
            );
            sys::Cronet_RequestFinishedInfoListener_SetClientContext(
                finished_listener,
                context_raw.cast_mut().cast(),
            );
        }
        // SAFETY: constructor takes no borrowed state.
        let request = unsafe { sys::Cronet_UrlRequest_Create() };
        if request.is_null() {
            // SAFETY: all objects and raw Arc are uniquely owned here.
            unsafe {
                sys::Cronet_RequestFinishedInfoListener_Destroy(finished_listener);
                sys::Cronet_UrlRequestCallback_Destroy(callback);
                drop(Arc::from_raw(context_raw));
                sys::Cronet_UrlRequestParams_Destroy(params);
            }
            return Err(Error::AllocationFailed("URL request"));
        }
        context.control.set_raw(request);
        Ok(Self {
            request,
            callback,
            finished_listener,
            params,
            context: context_raw,
            control: context.control.clone(),
            upload: None,
            operation: Some(operation),
            started: false,
        })
    }

    fn add_header(&mut self, header: &Header) -> Result<()> {
        // SAFETY: constructor takes no borrowed state.
        let raw = unsafe { sys::Cronet_HttpHeader_Create() };
        if raw.is_null() {
            return Err(Error::AllocationFailed("HTTP header"));
        }
        let name = header.c_name();
        let value = header.c_value();
        // SAFETY: setters/add copy their values.
        unsafe {
            sys::Cronet_HttpHeader_name_set(raw, name.as_ptr());
            sys::Cronet_HttpHeader_value_set(raw, value.as_ptr());
            sys::Cronet_UrlRequestParams_request_headers_add(self.params, raw);
            sys::Cronet_HttpHeader_Destroy(raw);
        }
        Ok(())
    }

    fn set_upload(
        &mut self,
        body: UploadBody,
        request_error: Arc<Mutex<Option<Error>>>,
    ) -> Result<()> {
        let upload = UploadProvider::new(body, self.operation().handle().clone(), request_error)?;
        // SAFETY: provider is retained by self through terminal completion.
        unsafe {
            sys::Cronet_UrlRequestParams_upload_data_provider_set(self.params, upload.raw);
            sys::Cronet_UrlRequestParams_upload_data_provider_executor_set(
                self.params,
                self.operation().executor(),
            );
        }
        self.upload = Some(upload);
        Ok(())
    }

    fn destroy_params(&mut self) {
        if !self.params.is_null() {
            // SAFETY: Cronet copied params during initialization.
            unsafe { sys::Cronet_UrlRequestParams_Destroy(self.params) };
            self.params = ptr::null_mut();
        }
    }

    fn operation(&self) -> &EngineOperation {
        self.operation
            .as_ref()
            .expect("live native request owns an engine operation")
    }
}

impl Drop for NativeRequest {
    fn drop(&mut self) {
        self.destroy_params();
        if self.started && !self.control.is_done() {
            // Tokio normally keeps this private cleanup task alive. If its
            // runtime is forcefully torn down, cancel and leak the still-live
            // native graph rather than freeing objects Cronet may still use.
            self.control.cancel();
            self.request = ptr::null_mut();
            self.callback = ptr::null_mut();
            self.finished_listener = ptr::null_mut();
            self.context = ptr::null();
            if let Some(upload) = self.upload.take() {
                std::mem::forget(upload);
            }
            if let Some(operation) = self.operation.take() {
                std::mem::forget(operation);
            }
            return;
        }
        if !self.request.is_null() {
            self.control.destroy_request(self.request);
            self.request = ptr::null_mut();
        }
        if !self.callback.is_null() {
            // SAFETY: request no longer references callback.
            unsafe { sys::Cronet_UrlRequestCallback_Destroy(self.callback) };
            self.callback = ptr::null_mut();
        }
        if !self.finished_listener.is_null() {
            // SAFETY: request no longer references listener.
            unsafe { sys::Cronet_RequestFinishedInfoListener_Destroy(self.finished_listener) };
            self.finished_listener = ptr::null_mut();
        }
        self.upload.take();
        if !self.context.is_null() {
            // SAFETY: reverses Arc::into_raw in new.
            let context = unsafe { Arc::from_raw(self.context) };
            let state = context
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !state.buffer.is_null() {
                // SAFETY: terminal completion returned buffer ownership to app.
                unsafe { sys::Cronet_Buffer_Destroy(state.buffer) };
            }
            drop(state);
            drop(context);
            self.context = ptr::null();
        }
    }
}

struct Annotation {
    token: Arc<u8>,
    value: String,
}
impl Annotation {
    fn new(value: String) -> Self {
        Self {
            token: Arc::new(0),
            value,
        }
    }
    fn tracking() -> Self {
        Self::new(String::new())
    }
    fn pointer(&self) -> sys::Cronet_RawDataPtr {
        Arc::as_ptr(&self.token).cast_mut().cast::<c_void>()
    }
}

struct CallbackContext {
    state: Mutex<CallbackState>,
    callback_count: AtomicUsize,
    callbacks_idle: tokio::sync::Notify,
    headers_sender: Mutex<Option<oneshot::Sender<Result<ResponseInfo>>>>,
    body_sender: mpsc::Sender<Result<Bytes>>,
    terminal_sender: Mutex<Option<oneshot::Sender<()>>>,
    finished_sender: watch::Sender<Option<RequestFinishedInfo>>,
    redirect_policy: RedirectPolicy,
    buffer_size: usize,
    max_response_bytes: usize,
    handle: Handle,
    control: Arc<RequestControl>,
    request_error: Arc<Mutex<Option<Error>>>,
    tracking_annotation: Annotation,
    annotations: Vec<Annotation>,
}

struct CallbackState {
    buffer: sys::Cronet_BufferPtr,
    received: usize,
    terminal: bool,
}

impl Default for CallbackState {
    fn default() -> Self {
        Self {
            buffer: ptr::null_mut(),
            received: 0,
            terminal: false,
        }
    }
}

// SAFETY: buffer ownership crosses Cronet's network thread and Tokio executor;
// all Rust access is serialized through CallbackContext::state.
unsafe impl Send for CallbackState {}

impl CallbackContext {
    async fn wait_for_callbacks(&self) {
        while self.callback_count.load(Ordering::Acquire) != 0 {
            let notified = self.callbacks_idle.notified();
            if self.callback_count.load(Ordering::Acquire) != 0 {
                notified.await;
            }
        }
    }

    fn own_buffer(&self, buffer: sys::Cronet_BufferPtr) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .buffer = buffer;
    }

    fn read(self: &Arc<Self>, request: sys::Cronet_UrlRequestPtr, buffer: sys::Cronet_BufferPtr) {
        // SAFETY: buffer is app-owned here and request is active.
        let result = unsafe { sys::Cronet_UrlRequest_Read(request, buffer) };
        let code = ResultCode::from_raw(result);
        if code != Some(ResultCode::UnexpectedRead) {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.buffer == buffer {
                state.buffer = ptr::null_mut();
            }
        }
        if let Some(code) = code {
            self.fail_then_cancel(request, Error::Cronet(code));
        }
    }

    fn fail_then_cancel(&self, request: sys::Cronet_UrlRequestPtr, error: Error) {
        let mut pending = self
            .request_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if pending.is_none() {
            *pending = Some(error);
        }
        drop(pending);
        // SAFETY: callback provides active request.
        unsafe { sys::Cronet_UrlRequest_Cancel(request) };
    }

    fn fail_from_task(&self, error: Error) {
        let mut pending = self
            .request_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if pending.is_none() {
            *pending = Some(error);
        }
        drop(pending);
        self.control.cancel();
    }

    fn resume_read(self: &Arc<Self>, buffer: sys::Cronet_BufferPtr) {
        let gate = self
            .control
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let request = self.control.raw.load(Ordering::Acquire) as sys::Cronet_UrlRequestPtr;
        if request.is_null() || self.control.is_done() {
            return;
        }
        self.read(request, buffer);
        drop(gate);
    }

    fn terminal(&self, fallback: Option<Error>) {
        let first = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.terminal {
                false
            } else {
                state.terminal = true;
                true
            }
        };
        if !first {
            return;
        }
        self.control.done.store(true, Ordering::Release);
        let error = self
            .request_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .or(fallback);
        if let Some(error) = error {
            if let Some(sender) = self
                .headers_sender
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                let _ = sender.send(Err(error.clone()));
            }
            let sender = self.body_sender.clone();
            self.handle.spawn(async move {
                let _ = sender.send(Err(error)).await;
            });
        } else if let Some(sender) = self
            .headers_sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = sender.send(Err(Error::CallbackChannelClosed));
        }
        if let Some(sender) = self
            .terminal_sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = sender.send(());
        }
    }

    fn annotation_map(&self) -> Vec<(*mut c_void, String)> {
        self.annotations
            .iter()
            .map(|annotation| (annotation.pointer(), annotation.value.clone()))
            .collect()
    }

    fn engine_annotations(&self) -> Vec<(usize, Arc<u8>, Option<String>)> {
        std::iter::once((
            self.tracking_annotation.pointer() as usize,
            self.tracking_annotation.token.clone(),
            None,
        ))
        .chain(self.annotations.iter().map(|annotation| {
            (
                annotation.pointer() as usize,
                annotation.token.clone(),
                Some(annotation.value.clone()),
            )
        }))
        .collect()
    }
}

unsafe fn callback_context(callback: sys::Cronet_UrlRequestCallbackPtr) -> Option<CallbackLease> {
    // SAFETY: callback is our live native object.
    let raw = unsafe { sys::Cronet_UrlRequestCallback_GetClientContext(callback) }
        .cast::<CallbackContext>();
    clone_raw_arc(raw).map(CallbackLease::new)
}

unsafe fn finished_context(
    listener: sys::Cronet_RequestFinishedInfoListenerPtr,
) -> Option<CallbackLease> {
    // SAFETY: listener is our live native object.
    let raw = unsafe { sys::Cronet_RequestFinishedInfoListener_GetClientContext(listener) }
        .cast::<CallbackContext>();
    clone_raw_arc(raw).map(CallbackLease::new)
}

struct CallbackLease(Arc<CallbackContext>);

impl CallbackLease {
    fn new(context: Arc<CallbackContext>) -> Self {
        context.callback_count.fetch_add(1, Ordering::AcqRel);
        Self(context)
    }

    fn arc(&self) -> Arc<CallbackContext> {
        self.0.clone()
    }
}

impl std::ops::Deref for CallbackLease {
    type Target = CallbackContext;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for CallbackLease {
    fn drop(&mut self) {
        if self.0.callback_count.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.callbacks_idle.notify_waiters();
        }
    }
}

fn clone_raw_arc<T>(raw: *const T) -> Option<Arc<T>> {
    if raw.is_null() {
        None
    } else {
        // SAFETY: native owner retains the original strong count.
        unsafe {
            Arc::increment_strong_count(raw);
            Some(Arc::from_raw(raw))
        }
    }
}

unsafe extern "C" fn on_redirect_received(
    callback: sys::Cronet_UrlRequestCallbackPtr,
    request: sys::Cronet_UrlRequestPtr,
    info: sys::Cronet_UrlResponseInfoPtr,
    location: sys::Cronet_String,
) {
    let Some(context) = (unsafe { callback_context(callback) }) else {
        return;
    };
    // SAFETY: callback-owned values are copied before returning.
    let redirect = RedirectInfo {
        response: unsafe { copy_response_info(info) },
        location: unsafe { copy_c_string(location) },
    };
    match &context.redirect_policy {
        RedirectPolicy::Follow => {
            if let Err(error) = context.control.follow_redirect() {
                context.fail_then_cancel(request, error);
            }
        }
        RedirectPolicy::Cancel => {
            context.fail_then_cancel(
                request,
                Error::Redirect {
                    location: redirect.location,
                    response: Box::new(redirect.response),
                },
            );
        }
        RedirectPolicy::Handler(handler) => {
            let handler = handler.clone();
            let handler_redirect = redirect.clone();
            let mut decision = context
                .handle
                .spawn(async move { handler(handler_redirect).await });
            let task_context = context.arc();
            let mut finished = context.finished_sender.subscribe();
            context.handle.spawn(async move {
                tokio::select! {
                    result = &mut decision => match result {
                        Ok(RedirectAction::Follow) => {
                            if let Err(error) = task_context.control.follow_redirect() {
                                task_context.fail_from_task(error);
                            }
                        }
                        Ok(RedirectAction::Cancel) => {
                            task_context.fail_from_task(Error::Redirect {
                                location: redirect.location,
                                response: Box::new(redirect.response),
                            });
                        }
                        Err(error) => task_context.fail_from_task(Error::TokioTask(format!(
                            "redirect handler did not complete: {error}"
                        ))),
                    },
                    () = wait_for_request_finished(&mut finished) => decision.abort(),
                }
            });
        }
    }
}

async fn wait_for_request_finished(finished: &mut watch::Receiver<Option<RequestFinishedInfo>>) {
    while finished.borrow().is_none() {
        if finished.changed().await.is_err() {
            return;
        }
    }
}

unsafe extern "C" fn on_response_started(
    callback: sys::Cronet_UrlRequestCallbackPtr,
    request: sys::Cronet_UrlRequestPtr,
    info: sys::Cronet_UrlResponseInfoPtr,
) {
    let Some(context) = (unsafe { callback_context(callback) }) else {
        return;
    };
    // SAFETY: callback-owned metadata is copied immediately.
    let info = unsafe { copy_response_info(info) };
    if let Some(sender) = context
        .headers_sender
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    {
        if sender.send(Ok(info)).is_err() {
            context.fail_then_cancel(request, Error::Canceled);
            return;
        }
    }
    // SAFETY: constructor takes no borrowed state.
    let buffer = unsafe { sys::Cronet_Buffer_Create() };
    if buffer.is_null() {
        context.fail_then_cancel(request, Error::AllocationFailed("read buffer"));
        return;
    }
    let size = u64::try_from(context.buffer_size).expect("builder validated size");
    // SAFETY: buffer is exclusively initialized here.
    unsafe { sys::Cronet_Buffer_InitWithAlloc(buffer, size) };
    context.own_buffer(buffer);
    context.0.read(request, buffer);
}

unsafe extern "C" fn on_read_completed(
    callback: sys::Cronet_UrlRequestCallbackPtr,
    request: sys::Cronet_UrlRequestPtr,
    _info: sys::Cronet_UrlResponseInfoPtr,
    buffer: sys::Cronet_BufferPtr,
    bytes_read: u64,
) {
    let Some(context) = (unsafe { callback_context(callback) }) else {
        return;
    };
    context.own_buffer(buffer);
    // SAFETY: callback provides live buffer.
    let capacity = unsafe { sys::Cronet_Buffer_GetSize(buffer) };
    if bytes_read > capacity {
        context.fail_then_cancel(
            request,
            Error::InvalidReadSize {
                reported: bytes_read,
                capacity,
            },
        );
        return;
    }
    let Ok(count) = usize::try_from(bytes_read) else {
        context.fail_then_cancel(
            request,
            Error::ResponseTooLarge {
                limit: context.max_response_bytes,
            },
        );
        return;
    };
    {
        let mut state = context
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let too_large = state
            .received
            .checked_add(count)
            .is_none_or(|total| total > context.max_response_bytes);
        if too_large {
            drop(state);
            context.fail_then_cancel(
                request,
                Error::ResponseTooLarge {
                    limit: context.max_response_bytes,
                },
            );
            return;
        }
        state.received += count;
    }
    let chunk = if count == 0 {
        Bytes::new()
    } else {
        // SAFETY: Cronet initialized count bytes and count <= capacity.
        let data = unsafe { sys::Cronet_Buffer_GetData(buffer) }.cast::<u8>();
        if data.is_null() {
            context.fail_then_cancel(request, Error::AllocationFailed("read buffer data"));
            return;
        }
        // SAFETY: data points to count readable bytes for this callback.
        Bytes::copy_from_slice(unsafe { std::slice::from_raw_parts(data, count) })
    };
    if chunk.is_empty() {
        context.0.read(request, buffer);
        return;
    }
    let buffer = buffer as usize;
    let sender = context.body_sender.clone();
    let task_context = context.arc();
    context.handle.spawn(async move {
        if sender.send(Ok(chunk)).await.is_err() {
            task_context.fail_from_task(Error::Canceled);
        } else {
            task_context.resume_read(buffer as sys::Cronet_BufferPtr);
        }
    });
}

unsafe extern "C" fn on_succeeded(
    callback: sys::Cronet_UrlRequestCallbackPtr,
    _request: sys::Cronet_UrlRequestPtr,
    _info: sys::Cronet_UrlResponseInfoPtr,
) {
    if let Some(context) = unsafe { callback_context(callback) } {
        context.terminal(None);
    }
}

unsafe extern "C" fn on_failed(
    callback: sys::Cronet_UrlRequestCallbackPtr,
    _request: sys::Cronet_UrlRequestPtr,
    _info: sys::Cronet_UrlResponseInfoPtr,
    error: sys::Cronet_ErrorPtr,
) {
    if let Some(context) = unsafe { callback_context(callback) } {
        // SAFETY: callback-owned error is copied immediately.
        context.terminal(Some(Error::Network(unsafe { copy_network_error(error) })));
    }
}

unsafe extern "C" fn on_canceled(
    callback: sys::Cronet_UrlRequestCallbackPtr,
    _request: sys::Cronet_UrlRequestPtr,
    _info: sys::Cronet_UrlResponseInfoPtr,
) {
    if let Some(context) = unsafe { callback_context(callback) } {
        context.terminal(Some(Error::Canceled));
    }
}

unsafe extern "C" fn on_request_finished(
    listener: sys::Cronet_RequestFinishedInfoListenerPtr,
    request_info: sys::Cronet_RequestFinishedInfoPtr,
    response_info: sys::Cronet_UrlResponseInfoPtr,
    error: sys::Cronet_ErrorPtr,
) {
    let Some(context) = (unsafe { finished_context(listener) }) else {
        return;
    };
    // SAFETY: all callback-owned objects are copied during this call.
    let info = unsafe {
        copy_finished_info(
            request_info,
            response_info,
            error,
            &context.annotation_map(),
        )
    };
    context.finished_sender.send_replace(Some(info));
}

struct UploadProvider {
    raw: sys::Cronet_UploadDataProviderPtr,
    context: *const UploadContext,
}

// SAFETY: the provider is owned by one NativeRequest cleanup task; callback
// state is Arc-backed and synchronized.
unsafe impl Send for UploadProvider {}

enum UploadSource {
    Bytes {
        body: Bytes,
        cursor: usize,
    },
    Reader {
        reader: Pin<Box<dyn AsyncRead + Send>>,
        length: Option<u64>,
        remaining: Option<u64>,
    },
    Rewindable {
        reader: Pin<Box<dyn AsyncReadSeek + Send>>,
        length: Option<u64>,
        remaining: Option<u64>,
    },
}

struct UploadContext {
    source: tokio::sync::Mutex<UploadSource>,
    length: i64,
    handle: Handle,
    closed: watch::Sender<bool>,
    completion_gate: Mutex<()>,
    request_error: Arc<Mutex<Option<Error>>>,
}

impl UploadProvider {
    fn new(
        body: UploadBody,
        handle: Handle,
        request_error: Arc<Mutex<Option<Error>>>,
    ) -> Result<Self> {
        let length = upload_body_length(&body)
            .map(i64::try_from)
            .transpose()
            .map_err(|_| Error::InvalidConfiguration("upload length does not fit int64_t"))?
            .unwrap_or(-1);
        let source = match body {
            UploadBody::Bytes(body) => UploadSource::Bytes { body, cursor: 0 },
            UploadBody::Reader { reader, length } => UploadSource::Reader {
                reader,
                length,
                remaining: length,
            },
            UploadBody::Rewindable { reader, length } => UploadSource::Rewindable {
                reader,
                length,
                remaining: length,
            },
        };
        // SAFETY: callbacks have required ABI.
        let raw = unsafe {
            sys::Cronet_UploadDataProvider_CreateWith(
                Some(upload_length),
                Some(upload_read),
                Some(upload_rewind),
                Some(upload_close),
            )
        };
        if raw.is_null() {
            return Err(Error::AllocationFailed("upload provider"));
        }
        let (closed, _) = watch::channel(false);
        let context = Arc::into_raw(Arc::new(UploadContext {
            source: tokio::sync::Mutex::new(source),
            length,
            handle,
            closed,
            completion_gate: Mutex::new(()),
            request_error,
        }));
        // SAFETY: raw Arc is retained until provider drop.
        unsafe {
            sys::Cronet_UploadDataProvider_SetClientContext(raw, context.cast_mut().cast());
        }
        Ok(Self { raw, context })
    }
}

impl Drop for UploadProvider {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            // SAFETY: native request no longer references provider.
            unsafe { sys::Cronet_UploadDataProvider_Destroy(self.raw) };
            self.raw = ptr::null_mut();
        }
        if !self.context.is_null() {
            // SAFETY: reverses Arc::into_raw in new.
            unsafe { drop(Arc::from_raw(self.context)) };
            self.context = ptr::null();
        }
    }
}

unsafe fn upload_context(
    provider: sys::Cronet_UploadDataProviderPtr,
) -> Option<Arc<UploadContext>> {
    // SAFETY: provider is our live native object.
    let raw = unsafe { sys::Cronet_UploadDataProvider_GetClientContext(provider) }
        .cast::<UploadContext>();
    clone_raw_arc(raw)
}

unsafe extern "C" fn upload_length(provider: sys::Cronet_UploadDataProviderPtr) -> i64 {
    unsafe { upload_context(provider) }.map_or(-1, |context| context.length)
}

unsafe extern "C" fn upload_read(
    provider: sys::Cronet_UploadDataProviderPtr,
    sink: sys::Cronet_UploadDataSinkPtr,
    buffer: sys::Cronet_BufferPtr,
) {
    let Some(context) = (unsafe { upload_context(provider) }) else {
        upload_read_error(sink);
        return;
    };
    // SAFETY: callback provides live buffer.
    let capacity = unsafe { sys::Cronet_Buffer_GetSize(buffer) };
    let Ok(capacity) = usize::try_from(capacity) else {
        upload_read_error(sink);
        return;
    };
    let sink = sink as usize;
    let buffer = buffer as usize;
    let task_context = context.clone();
    context.handle.spawn(async move {
        let mut closed = task_context.closed.subscribe();
        let mut temporary = Vec::new();
        if temporary.try_reserve_exact(capacity).is_err() {
            finish_upload_error(
                &task_context,
                sink,
                Error::AllocationFailed("upload buffer"),
            );
            return;
        }
        temporary.resize(capacity, 0);
        let read_result = tokio::select! {
            biased;
            () = wait_for_upload_close(&mut closed) => Err(Error::Canceled),
            result = async {
                let mut source = task_context.source.lock().await;
                match &mut *source {
                    UploadSource::Bytes { body, cursor } => {
                        let count = body.len().saturating_sub(*cursor).min(capacity);
                        temporary[..count].copy_from_slice(&body[*cursor..*cursor + count]);
                        *cursor += count;
                        Ok((count, false))
                    }
                    UploadSource::Reader {
                        reader,
                        length,
                        remaining,
                    } => read_upload_source(reader.as_mut(), *length, remaining, &mut temporary).await,
                    UploadSource::Rewindable {
                        reader,
                        length,
                        remaining,
                    } => read_upload_source(reader.as_mut(), *length, remaining, &mut temporary).await,
                }
            } => result,
        };
        let (count, final_chunk) = match read_result {
            Ok(value) => value,
            Err(error) => {
                finish_upload_error(&task_context, sink, error);
                return;
            }
        };
        let gate = task_context
            .completion_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if count != 0 {
            // SAFETY: Cronet retains the callback buffer until the mandatory
            // sink completion, including when cancellation happened meanwhile.
            let destination =
                unsafe { sys::Cronet_Buffer_GetData(buffer as sys::Cronet_BufferPtr) }.cast::<u8>();
            if destination.is_null() {
                drop(gate);
                finish_upload_error(
                    &task_context,
                    sink,
                    Error::AllocationFailed("upload buffer data"),
                );
                return;
            }
            // SAFETY: both ranges contain count bytes and do not overlap.
            unsafe { ptr::copy_nonoverlapping(temporary.as_ptr(), destination, count) };
        }
        // SAFETY: sink is live until this completion call.
        unsafe {
            sys::Cronet_UploadDataSink_OnReadSucceeded(
                sink as sys::Cronet_UploadDataSinkPtr,
                u64::try_from(count).expect("usize fits uint64_t"),
                final_chunk,
            );
        }
    });
}

unsafe extern "C" fn upload_rewind(
    provider: sys::Cronet_UploadDataProviderPtr,
    sink: sys::Cronet_UploadDataSinkPtr,
) {
    let Some(context) = (unsafe { upload_context(provider) }) else {
        upload_rewind_error(sink);
        return;
    };
    let sink = sink as usize;
    let task_context = context.clone();
    context.handle.spawn(async move {
        let mut closed = task_context.closed.subscribe();
        let result = tokio::select! {
            biased;
            () = wait_for_upload_close(&mut closed) => Err(Error::Canceled),
            result = async {
                let mut source = task_context.source.lock().await;
                match &mut *source {
                    UploadSource::Bytes { cursor, .. } => {
                        *cursor = 0;
                        Ok(())
                    }
                    UploadSource::Reader { .. } => {
                        Err(Error::Upload("upload source is not rewindable".to_owned()))
                    }
                    UploadSource::Rewindable {
                        reader,
                        length,
                        remaining,
                    } => {
                        let result = reader
                            .as_mut()
                            .seek(SeekFrom::Start(0))
                            .await
                            .map(|_| ())
                            .map_err(|error| Error::Upload(error.to_string()));
                        if result.is_ok() {
                            *remaining = *length;
                        }
                        result
                    }
                }
            } => result,
        };
        let _gate = task_context
            .completion_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match result {
            Ok(()) => unsafe {
                sys::Cronet_UploadDataSink_OnRewindSucceeded(sink as sys::Cronet_UploadDataSinkPtr);
            },
            Err(error) => {
                record_upload_error(&task_context, error);
                upload_rewind_error(sink as sys::Cronet_UploadDataSinkPtr);
            }
        }
    });
}

unsafe extern "C" fn upload_close(provider: sys::Cronet_UploadDataProviderPtr) {
    // A pending Read/Rewind must still report exactly one sink result after
    // cancellation. NativeRequest retains the provider and Rust source until
    // all terminal cleanup is complete, so Close does not release them early.
    if let Some(context) = unsafe { upload_context(provider) } {
        context.closed.send_replace(true);
    }
}

async fn wait_for_upload_close(closed: &mut watch::Receiver<bool>) {
    while !*closed.borrow() {
        if closed.changed().await.is_err() {
            return;
        }
    }
}

async fn read_upload_source<R: AsyncRead + ?Sized>(
    mut reader: Pin<&mut R>,
    declared_length: Option<u64>,
    remaining: &mut Option<u64>,
    output: &mut [u8],
) -> Result<(usize, bool)> {
    let limit = remaining
        .and_then(|remaining| usize::try_from(remaining).ok())
        .map_or(output.len(), |remaining| remaining.min(output.len()));
    let count = reader
        .as_mut()
        .read(&mut output[..limit])
        .await
        .map_err(|error| Error::Upload(error.to_string()))?;
    if count == 0 && remaining.is_some_and(|remaining| remaining != 0) {
        return Err(Error::Upload(
            "upload source ended before its declared content length".to_owned(),
        ));
    }
    if let Some(remaining) = remaining {
        *remaining = remaining.saturating_sub(count as u64);
    }
    Ok((count, declared_length.is_none() && count == 0))
}

fn finish_upload_error(context: &UploadContext, sink: usize, error: Error) {
    let _gate = context
        .completion_gate
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    record_upload_error(context, error);
    upload_read_error(sink as sys::Cronet_UploadDataSinkPtr);
}

fn record_upload_error(context: &UploadContext, error: Error) {
    let mut pending = context
        .request_error
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if pending.is_none() {
        *pending = Some(error);
    }
}

fn upload_read_error(sink: sys::Cronet_UploadDataSinkPtr) {
    const MESSAGE: &[u8] = b"Rust async upload provider failed\0";
    if !sink.is_null() {
        // SAFETY: static message is NUL terminated; callback sink is live.
        unsafe { sys::Cronet_UploadDataSink_OnReadError(sink, MESSAGE.as_ptr().cast()) };
    }
}

fn upload_rewind_error(sink: sys::Cronet_UploadDataSinkPtr) {
    const MESSAGE: &[u8] = b"Rust async upload source cannot rewind\0";
    if !sink.is_null() {
        // SAFETY: static message is NUL terminated; callback sink is live.
        unsafe { sys::Cronet_UploadDataSink_OnRewindError(sink, MESSAGE.as_ptr().cast()) };
    }
}

const fn priority_raw(value: Priority) -> sys::Cronet_UrlRequestParams_REQUEST_PRIORITY {
    match value {
        Priority::Idle => sys::Cronet_UrlRequestParams_REQUEST_PRIORITY_REQUEST_PRIORITY_IDLE,
        Priority::Lowest => sys::Cronet_UrlRequestParams_REQUEST_PRIORITY_REQUEST_PRIORITY_LOWEST,
        Priority::Low => sys::Cronet_UrlRequestParams_REQUEST_PRIORITY_REQUEST_PRIORITY_LOW,
        Priority::Medium => sys::Cronet_UrlRequestParams_REQUEST_PRIORITY_REQUEST_PRIORITY_MEDIUM,
        Priority::Highest => sys::Cronet_UrlRequestParams_REQUEST_PRIORITY_REQUEST_PRIORITY_HIGHEST,
    }
}

const fn idempotency_raw(value: Idempotency) -> sys::Cronet_UrlRequestParams_IDEMPOTENCY {
    match value {
        Idempotency::Default => sys::Cronet_UrlRequestParams_IDEMPOTENCY_DEFAULT_IDEMPOTENCY,
        Idempotency::Idempotent => sys::Cronet_UrlRequestParams_IDEMPOTENCY_IDEMPOTENT,
        Idempotency::NotIdempotent => sys::Cronet_UrlRequestParams_IDEMPOTENCY_NOT_IDEMPOTENT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_tokio_body<T: AsyncRead + Stream<Item = Result<Bytes>> + Send + Unpin>() {}
    fn assert_send_future<T: Future<Output = Result<StreamingResponse>> + Send + Unpin>() {}

    #[test]
    fn validates_builder_without_native_calls() {
        assert_tokio_body::<ResponseBody>();
        assert_send_future::<PendingRequest>();
        let request = Request::builder("https://example.com")
            .unwrap()
            .method("POST")
            .unwrap()
            .header("content-type", "text/plain")
            .unwrap()
            .annotation("trace-42")
            .unwrap()
            .body(Bytes::from_static(b"hello"))
            .max_response_bytes(1024)
            .build()
            .unwrap();
        assert_eq!(request.method.as_deref(), Some("POST"));
        assert!(
            matches!(request.body, Some(UploadBody::Bytes(ref body)) if body.as_ref() == b"hello")
        );
    }

    #[test]
    fn rejects_invalid_buffer_configuration() {
        assert!(
            Request::builder("https://example.com")
                .unwrap()
                .read_buffer_size(0)
                .build()
                .is_err()
        );
        assert!(
            Request::builder("https://example.com")
                .unwrap()
                .body_channel_capacity(0)
                .build()
                .is_err()
        );
        assert!(
            Request::builder("https://example.com")
                .unwrap()
                .read_buffer_size(i32::MAX as usize + 1)
                .build()
                .is_err()
        );
    }

    #[tokio::test]
    async fn validates_declared_and_chunked_upload_completion() {
        let mut short = std::io::Cursor::new(b"short".to_vec());
        let mut remaining = Some(8);
        let mut output = [0; 8];
        let first = read_upload_source(Pin::new(&mut short), Some(8), &mut remaining, &mut output)
            .await
            .unwrap();
        assert_eq!(first, (5, false));
        assert!(
            read_upload_source(Pin::new(&mut short), Some(8), &mut remaining, &mut output,)
                .await
                .is_err()
        );

        let mut chunked = std::io::Cursor::new(Vec::<u8>::new());
        let mut remaining = None;
        assert_eq!(
            read_upload_source(Pin::new(&mut chunked), None, &mut remaining, &mut output,)
                .await
                .unwrap(),
            (0, true)
        );
    }
}
