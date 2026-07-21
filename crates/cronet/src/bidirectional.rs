use std::{
    collections::{HashMap, VecDeque},
    ffi::{CStr, CString, c_char, c_void},
    fmt,
    future::Future,
    io,
    pin::Pin,
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
};

use bytes::{Buf, Bytes};
use cronet_sys as sys;
use futures_core::Stream;
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::{Notify, mpsc, oneshot, watch},
};

use crate::{
    Engine, Error, Header, Priority, Result,
    engine::{EngineOperation, RequestCanceler},
    types::{copy_c_string, to_cstring, validate_string},
};

const DEFAULT_READ_BUFFER_SIZE: usize = 32 * 1024;
const DEFAULT_READ_CHANNEL_CAPACITY: usize = 8;
const DEFAULT_WRITE_CAPACITY: usize = 8;

/// Initial configuration for Cronet's HTTP/2 or QUIC full-duplex stream.
pub struct BidirectionalRequest {
    url: String,
    method: String,
    headers: Vec<Header>,
    priority: Priority,
    read_buffer_size: usize,
    read_channel_capacity: usize,
    write_capacity: usize,
    disable_auto_flush: bool,
    delay_headers_until_flush: bool,
    end_of_stream: bool,
}

impl fmt::Debug for BidirectionalRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BidirectionalRequest")
            .field("url", &self.url)
            .field("method", &self.method)
            .field("headers", &self.headers)
            .field("priority", &self.priority)
            .field("read_buffer_size", &self.read_buffer_size)
            .field("read_channel_capacity", &self.read_channel_capacity)
            .field("write_capacity", &self.write_capacity)
            .field("disable_auto_flush", &self.disable_auto_flush)
            .field("delay_headers_until_flush", &self.delay_headers_until_flush)
            .field("end_of_stream", &self.end_of_stream)
            .finish()
    }
}

impl BidirectionalRequest {
    pub fn builder(url: impl Into<String>) -> Result<BidirectionalRequestBuilder> {
        BidirectionalRequestBuilder::new(url)
    }
}

/// Builder for a Cronet bidirectional stream.
#[derive(Debug)]
pub struct BidirectionalRequestBuilder {
    request: BidirectionalRequest,
}

impl BidirectionalRequestBuilder {
    pub fn new(url: impl Into<String>) -> Result<Self> {
        let url = url.into();
        validate_string(&url, "bidirectional stream URL")?;
        Ok(Self {
            request: BidirectionalRequest {
                url,
                method: "POST".to_owned(),
                headers: Vec::new(),
                priority: Priority::Medium,
                read_buffer_size: DEFAULT_READ_BUFFER_SIZE,
                read_channel_capacity: DEFAULT_READ_CHANNEL_CAPACITY,
                write_capacity: DEFAULT_WRITE_CAPACITY,
                disable_auto_flush: false,
                delay_headers_until_flush: false,
                end_of_stream: false,
            },
        })
    }

    pub fn method(mut self, method: impl Into<String>) -> Result<Self> {
        let method = method.into();
        validate_string(&method, "bidirectional stream method")?;
        self.request.method = method;
        Ok(self)
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Result<Self> {
        self.request.headers.push(Header::new(name, value)?);
        Ok(self)
    }

    #[must_use]
    pub const fn priority(mut self, priority: Priority) -> Self {
        self.request.priority = priority;
        self
    }

    #[must_use]
    pub const fn read_buffer_size(mut self, bytes: usize) -> Self {
        self.request.read_buffer_size = bytes;
        self
    }

    #[must_use]
    pub const fn read_channel_capacity(mut self, chunks: usize) -> Self {
        self.request.read_channel_capacity = chunks;
        self
    }

    /// Bounds queued and native-in-flight write buffers.
    #[must_use]
    pub const fn write_capacity(mut self, chunks: usize) -> Self {
        self.request.write_capacity = chunks;
        self
    }

    /// Disables native auto-flush. Tokio `flush` or `shutdown` then flushes it.
    #[must_use]
    pub const fn disable_auto_flush(mut self, disable: bool) -> Self {
        self.request.disable_auto_flush = disable;
        self
    }

    /// Delays QUIC request headers until the first flush.
    #[must_use]
    pub const fn delay_headers_until_flush(mut self, delay: bool) -> Self {
        self.request.delay_headers_until_flush = delay;
        self
    }

    /// Starts a read-only stream with end-of-stream set on request headers.
    #[must_use]
    pub const fn end_of_stream(mut self, end: bool) -> Self {
        self.request.end_of_stream = end;
        self
    }

    pub fn build(self) -> Result<BidirectionalRequest> {
        if self.request.read_buffer_size == 0 || self.request.read_buffer_size > i32::MAX as usize {
            return Err(Error::InvalidConfiguration(
                "bidirectional read buffer must fit a positive native int",
            ));
        }
        if self.request.read_channel_capacity == 0 || self.request.write_capacity == 0 {
            return Err(Error::InvalidConfiguration(
                "bidirectional channel capacities must be greater than zero",
            ));
        }
        Ok(self.request)
    }
}

/// Response headers from a bidirectional HTTP/2 or QUIC stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BidirectionalResponseHeaders {
    pub headers: Vec<Header>,
    pub negotiated_protocol: String,
}

impl BidirectionalResponseHeaders {
    #[must_use]
    pub fn status(&self) -> Option<u16> {
        self.headers
            .iter()
            .find(|header| header.name() == ":status")
            .and_then(|header| header.value().parse().ok())
    }
}

enum ReadEvent {
    Chunk(Bytes),
    Error(Error),
    Eof,
}

/// Tokio full-duplex adapter over Chromium's gRPC bidirectional stream API.
///
/// It implements `AsyncRead`, `AsyncWrite`, and `Stream`. Do not poll the read
/// side through `AsyncRead` and `Stream` concurrently.
pub struct BidirectionalStream {
    receiver: mpsc::Receiver<ReadEvent>,
    current: Bytes,
    read_eof: bool,
    control: Arc<BidirectionalControl>,
    write: Arc<WriteShared>,
    headers: watch::Receiver<Option<Result<BidirectionalResponseHeaders>>>,
    trailers: watch::Receiver<Option<Result<Vec<Header>>>>,
    terminal: watch::Receiver<Option<Result<()>>>,
    flush_receiver: Option<oneshot::Receiver<Result<()>>>,
    shutdown_started: bool,
    local_closed: bool,
}

impl fmt::Debug for BidirectionalStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BidirectionalStream")
            .field("is_done", &self.control.is_done())
            .field("read_eof", &self.read_eof)
            .field("local_closed", &self.local_closed)
            .finish_non_exhaustive()
    }
}

impl BidirectionalStream {
    pub async fn response_headers(&mut self) -> Result<BidirectionalResponseHeaders> {
        loop {
            if let Some(result) = self.headers.borrow().clone() {
                return result;
            }
            self.headers
                .changed()
                .await
                .map_err(|_| Error::CallbackChannelClosed)?;
        }
    }

    /// Waits for response trailers. An empty vector means none were sent.
    pub async fn trailers(&mut self) -> Result<Vec<Header>> {
        loop {
            if let Some(result) = self.trailers.borrow().clone() {
                return result;
            }
            self.trailers
                .changed()
                .await
                .map_err(|_| Error::CallbackChannelClosed)?;
        }
    }

    /// Waits for successful, failed, or canceled terminal completion.
    pub async fn finished(&mut self) -> Result<()> {
        loop {
            if let Some(result) = self.terminal.borrow().clone() {
                return result;
            }
            self.terminal
                .changed()
                .await
                .map_err(|_| Error::CallbackChannelClosed)?;
        }
    }

    pub fn cancel(&self) {
        self.control.cancel();
    }

    #[must_use]
    pub fn is_done(&self) -> bool {
        self.control.is_done()
    }

    pub async fn next_chunk(&mut self) -> Option<Result<Bytes>> {
        match self.receiver.recv().await {
            Some(ReadEvent::Chunk(chunk)) => Some(Ok(chunk)),
            Some(ReadEvent::Error(error)) => Some(Err(error)),
            Some(ReadEvent::Eof) | None => {
                self.read_eof = true;
                None
            }
        }
    }

    fn poll_flush_receiver(&mut self, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let Some(receiver) = &mut self.flush_receiver else {
            return Poll::Ready(Ok(()));
        };
        match Pin::new(receiver).poll(context) {
            Poll::Ready(Ok(Ok(()))) => {
                self.flush_receiver = None;
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Ok(Err(error))) => {
                self.flush_receiver = None;
                Poll::Ready(Err(io::Error::other(error.to_string())))
            }
            Poll::Ready(Err(_)) => {
                self.flush_receiver = None;
                Poll::Ready(Err(io::Error::other("bidirectional writer stopped")))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Stream for BidirectionalStream {
    type Item = Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.current.has_remaining() {
            return Poll::Ready(Some(Ok(std::mem::take(&mut self.current))));
        }
        if self.read_eof {
            return Poll::Ready(None);
        }
        match self.receiver.poll_recv(context) {
            Poll::Ready(Some(ReadEvent::Chunk(chunk))) => Poll::Ready(Some(Ok(chunk))),
            Poll::Ready(Some(ReadEvent::Error(error))) => Poll::Ready(Some(Err(error))),
            Poll::Ready(Some(ReadEvent::Eof) | None) => {
                self.read_eof = true;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl AsyncRead for BidirectionalStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            if self.current.has_remaining() {
                let count = self.current.remaining().min(output.remaining());
                output.put_slice(&self.current[..count]);
                self.current.advance(count);
                return Poll::Ready(Ok(()));
            }
            if self.read_eof {
                return Poll::Ready(Ok(()));
            }
            match self.receiver.poll_recv(context) {
                Poll::Ready(Some(ReadEvent::Chunk(chunk))) => self.current = chunk,
                Poll::Ready(Some(ReadEvent::Error(error))) => {
                    return Poll::Ready(Err(io::Error::other(error.to_string())));
                }
                Poll::Ready(Some(ReadEvent::Eof) | None) => {
                    self.read_eof = true;
                    return Poll::Ready(Ok(()));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl AsyncWrite for BidirectionalStream {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        input: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.local_closed || this.shutdown_started {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "bidirectional write side is closed",
            )));
        }
        if input.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let count = input.len().min(i32::MAX as usize);
        match this.write.enqueue_data(
            Bytes::copy_from_slice(&input[..count]),
            false,
            context.waker(),
        ) {
            Ok(true) => Poll::Ready(Ok(count)),
            Ok(false) => Poll::Pending,
            Err(error) => Poll::Ready(Err(io::Error::other(error.to_string()))),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.flush_receiver.is_none() {
            let (sender, receiver) = oneshot::channel();
            if let Err(error) = self.write.enqueue_control(WriteCommand::Flush(sender)) {
                return Poll::Ready(Err(io::Error::other(error.to_string())));
            }
            self.flush_receiver = Some(receiver);
        }
        self.poll_flush_receiver(context)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.local_closed {
            return Poll::Ready(Ok(()));
        }
        if !self.shutdown_started {
            match self
                .write
                .enqueue_data(Bytes::from_static(b""), true, context.waker())
            {
                Ok(true) => {
                    self.shutdown_started = true;
                    let (sender, receiver) = oneshot::channel();
                    if let Err(error) = self.write.enqueue_control(WriteCommand::Flush(sender)) {
                        return Poll::Ready(Err(io::Error::other(error.to_string())));
                    }
                    self.flush_receiver = Some(receiver);
                }
                Ok(false) => return Poll::Pending,
                Err(error) => return Poll::Ready(Err(io::Error::other(error.to_string()))),
            }
        }
        match self.poll_flush_receiver(context) {
            Poll::Ready(Ok(())) => {
                self.local_closed = true;
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

impl Drop for BidirectionalStream {
    fn drop(&mut self) {
        if !self.control.is_done() {
            self.control.cancel();
        }
    }
}

#[allow(clippy::similar_names, clippy::too_many_lines)]
pub(crate) async fn open(
    engine: &Engine,
    request: BidirectionalRequest,
) -> Result<BidirectionalStream> {
    let BidirectionalRequest {
        url,
        method,
        headers,
        priority,
        read_buffer_size,
        read_channel_capacity,
        write_capacity,
        disable_auto_flush,
        delay_headers_until_flush,
        end_of_stream,
    } = request;
    let url = to_cstring(&url, "bidirectional stream URL")?;
    let method = to_cstring(&method, "bidirectional stream method")?;
    let operation = engine.inner.begin_operation()?;
    // SAFETY: operation keeps the native engine live for this call.
    let stream_engine = unsafe { sys::Cronet_Engine_GetStreamEngine(operation.raw()) };
    if stream_engine.is_null() {
        return Err(Error::AllocationFailed("bidirectional stream engine"));
    }

    let (ready_sender, ready_receiver) = oneshot::channel();
    let (read_sender, read_receiver) = mpsc::channel(read_channel_capacity);
    let (headers_sender, headers_receiver) = watch::channel(None);
    let (trailers_sender, trailers_receiver) = watch::channel(None);
    let (terminal_sender, terminal_receiver) = watch::channel(None);
    let (cleanup_sender, cleanup_receiver) = oneshot::channel();
    let control = Arc::new(BidirectionalControl::default());
    let write = Arc::new(WriteShared::new(write_capacity));
    let context = Arc::new(BidirectionalContext {
        handle: operation.handle().clone(),
        callback_count: AtomicUsize::new(0),
        callbacks_idle: Notify::new(),
        control: control.clone(),
        read: Mutex::new(ReadState {
            buffer: vec![0; read_buffer_size],
            in_flight: false,
            eof: false,
        }),
        read_sender,
        ready_sender: Mutex::new(Some(ready_sender)),
        headers_sender,
        trailers_sender,
        terminal_sender,
        cleanup_sender: Mutex::new(Some(cleanup_sender)),
        pending_error: Mutex::new(None),
        write: write.clone(),
    });
    let mut native = NativeBidirectionalStream::new(&context, operation, stream_engine)?;
    let canceler: Arc<dyn RequestCanceler> = control.clone();
    if engine.inner.register(&canceler) {
        return Err(Error::EngineShutdown);
    }

    // SAFETY: stream is newly created and configuration is applied before start.
    unsafe {
        sys::bidirectional_stream_disable_auto_flush(native.stream, disable_auto_flush);
        sys::bidirectional_stream_delay_request_headers_until_flush(
            native.stream,
            delay_headers_until_flush,
        );
    }
    let native_headers = NativeHeaders::new(&headers);
    // SAFETY: strings/header storage remains live for this synchronous call.
    let start_result = unsafe {
        sys::bidirectional_stream_start(
            native.stream,
            url.as_ptr(),
            priority_value(priority),
            method.as_ptr(),
            &raw const native_headers.array,
            end_of_stream,
        )
    };
    if start_result != 0 {
        return Err(Error::BidirectionalApi {
            operation: "start",
            code: start_result,
        });
    }
    native.started = true;
    control.started.store(true, Ordering::Release);
    if control.cancel_requested.load(Ordering::Acquire) {
        control.cancel();
    }

    let writer_control = control.clone();
    let writer_write = write.clone();
    context
        .handle
        .spawn(async move { write_loop(writer_control, writer_write).await });
    let cleanup_context = context.clone();
    context.handle.spawn(async move {
        let _ = cleanup_receiver.await;
        cleanup_context.wait_for_callbacks().await;
        drop(native);
    });

    let mut guard = CancelOnDrop(Some(control.clone()));
    ready_receiver
        .await
        .map_err(|_| Error::CallbackChannelClosed)??;
    guard.disarm();
    Ok(BidirectionalStream {
        receiver: read_receiver,
        current: Bytes::new(),
        read_eof: false,
        control,
        write,
        headers: headers_receiver,
        trailers: trailers_receiver,
        terminal: terminal_receiver,
        flush_receiver: None,
        shutdown_started: end_of_stream,
        local_closed: end_of_stream,
    })
}

struct CancelOnDrop(Option<Arc<BidirectionalControl>>);
impl CancelOnDrop {
    fn disarm(&mut self) {
        self.0.take();
    }
}
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(control) = &self.0 {
            control.cancel();
        }
    }
}

#[derive(Default)]
struct BidirectionalControl {
    raw: AtomicUsize,
    started: AtomicBool,
    done: AtomicBool,
    cancel_requested: AtomicBool,
    gate: Mutex<()>,
}

impl BidirectionalControl {
    fn is_done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    fn cancel(&self) {
        self.cancel_requested.store(true, Ordering::Release);
        let _gate = self
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let raw = self.raw.load(Ordering::Acquire) as *mut sys::bidirectional_stream;
        if !raw.is_null() && self.started.load(Ordering::Acquire) && !self.is_done() {
            // SAFETY: gate keeps raw stream alive through the call.
            unsafe { sys::bidirectional_stream_cancel(raw) };
        }
    }

    fn write(&self, data: &Bytes, end_of_stream: bool) -> Result<()> {
        let _gate = self
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let raw = self.raw.load(Ordering::Acquire) as *mut sys::bidirectional_stream;
        if raw.is_null() || self.is_done() {
            return Err(Error::Canceled);
        }
        let length = i32::try_from(data.len()).map_err(|_| {
            Error::InvalidConfiguration("bidirectional write does not fit a native int")
        })?;
        // SAFETY: in-flight map retains data until its completion callback.
        let result = unsafe {
            sys::bidirectional_stream_write(raw, data.as_ptr().cast(), length, end_of_stream)
        };
        if result == 1 {
            Ok(())
        } else {
            Err(Error::BidirectionalApi {
                operation: "write",
                code: result,
            })
        }
    }

    fn flush(&self) -> Result<()> {
        let _gate = self
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let raw = self.raw.load(Ordering::Acquire) as *mut sys::bidirectional_stream;
        if raw.is_null() || self.is_done() {
            return Err(Error::Canceled);
        }
        // SAFETY: gate keeps raw stream alive through the call.
        unsafe { sys::bidirectional_stream_flush(raw) };
        Ok(())
    }

    fn destroy(&self, raw: *mut sys::bidirectional_stream) {
        let _gate = self
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.raw.store(0, Ordering::Release);
        // SAFETY: cleanup only destroys before start or after terminal callback.
        unsafe { sys::bidirectional_stream_destroy(raw) };
    }
}

impl RequestCanceler for BidirectionalControl {
    fn cancel(&self) {
        BidirectionalControl::cancel(self);
    }
}

struct NativeBidirectionalStream {
    stream: *mut sys::bidirectional_stream,
    callback: Option<Box<sys::bidirectional_stream_callback>>,
    context: *const BidirectionalContext,
    control: Arc<BidirectionalControl>,
    operation: Option<EngineOperation>,
    started: bool,
}

// SAFETY: ownership moves to one cleanup task and all shared state is locked.
unsafe impl Send for NativeBidirectionalStream {}

impl NativeBidirectionalStream {
    fn new(
        context: &Arc<BidirectionalContext>,
        operation: EngineOperation,
        engine: *mut sys::stream_engine,
    ) -> Result<Self> {
        let callback = Box::new(sys::bidirectional_stream_callback {
            on_stream_ready: Some(on_stream_ready),
            on_response_headers_received: Some(on_response_headers),
            on_read_completed: Some(on_read_completed),
            on_write_completed: Some(on_write_completed),
            on_response_trailers_received: Some(on_response_trailers),
            on_succeded: Some(on_succeeded),
            on_failed: Some(on_failed),
            on_canceled: Some(on_canceled),
        });
        let context_raw = Arc::into_raw(context.clone());
        // SAFETY: callback and raw Arc remain live until native cleanup.
        let stream = unsafe {
            sys::bidirectional_stream_create(
                engine,
                context_raw.cast_mut().cast::<c_void>(),
                &raw const *callback,
            )
        };
        if stream.is_null() {
            // SAFETY: create did not retain annotation on failure.
            unsafe { drop(Arc::from_raw(context_raw)) };
            return Err(Error::AllocationFailed("bidirectional stream"));
        }
        context
            .control
            .raw
            .store(stream as usize, Ordering::Release);
        Ok(Self {
            stream,
            callback: Some(callback),
            context: context_raw,
            control: context.control.clone(),
            operation: Some(operation),
            started: false,
        })
    }
}

impl Drop for NativeBidirectionalStream {
    fn drop(&mut self) {
        if self.started && !self.control.is_done() {
            self.control.cancel();
            self.stream = ptr::null_mut();
            self.context = ptr::null();
            if let Some(callback) = self.callback.take() {
                std::mem::forget(callback);
            }
            if let Some(operation) = self.operation.take() {
                std::mem::forget(operation);
            }
            return;
        }
        if !self.stream.is_null() {
            self.control.destroy(self.stream);
            self.stream = ptr::null_mut();
        }
        if !self.context.is_null() {
            // SAFETY: reverses Arc::into_raw in new after terminal completion.
            unsafe { drop(Arc::from_raw(self.context)) };
            self.context = ptr::null();
        }
        self.callback.take();
    }
}

struct NativeHeaders {
    _names: Vec<CString>,
    _values: Vec<CString>,
    _headers: Vec<sys::bidirectional_stream_header>,
    array: sys::bidirectional_stream_header_array,
}

impl NativeHeaders {
    fn new(headers: &[Header]) -> Self {
        let names = headers.iter().map(Header::c_name).collect::<Vec<_>>();
        let values = headers.iter().map(Header::c_value).collect::<Vec<_>>();
        let mut native = names
            .iter()
            .zip(&values)
            .map(|(name, value)| sys::bidirectional_stream_header {
                key: name.as_ptr(),
                value: value.as_ptr(),
            })
            .collect::<Vec<_>>();
        let array = sys::bidirectional_stream_header_array {
            count: native.len(),
            capacity: native.len(),
            headers: native.as_mut_ptr(),
        };
        Self {
            _names: names,
            _values: values,
            _headers: native,
            array,
        }
    }
}

struct ReadState {
    buffer: Vec<u8>,
    in_flight: bool,
    eof: bool,
}

struct BidirectionalContext {
    handle: tokio::runtime::Handle,
    callback_count: AtomicUsize,
    callbacks_idle: Notify,
    control: Arc<BidirectionalControl>,
    read: Mutex<ReadState>,
    read_sender: mpsc::Sender<ReadEvent>,
    ready_sender: Mutex<Option<oneshot::Sender<Result<()>>>>,
    headers_sender: watch::Sender<Option<Result<BidirectionalResponseHeaders>>>,
    trailers_sender: watch::Sender<Option<Result<Vec<Header>>>>,
    terminal_sender: watch::Sender<Option<Result<()>>>,
    cleanup_sender: Mutex<Option<oneshot::Sender<()>>>,
    pending_error: Mutex<Option<Error>>,
    write: Arc<WriteShared>,
}

impl BidirectionalContext {
    async fn wait_for_callbacks(&self) {
        while self.callback_count.load(Ordering::Acquire) != 0 {
            let notified = self.callbacks_idle.notified();
            if self.callback_count.load(Ordering::Acquire) != 0 {
                notified.await;
            }
        }
    }

    fn start_read(self: &Arc<Self>) {
        let gate = self
            .control
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let raw = self.control.raw.load(Ordering::Acquire) as *mut sys::bidirectional_stream;
        if raw.is_null() || self.control.is_done() {
            return;
        }
        let mut read = self
            .read
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if read.in_flight || read.eof {
            return;
        }
        read.in_flight = true;
        let pointer = read.buffer.as_mut_ptr().cast::<c_char>();
        let capacity = i32::try_from(read.buffer.len()).expect("builder validated read size");
        // SAFETY: buffer remains stable in context until completion callback.
        let result = unsafe { sys::bidirectional_stream_read(raw, pointer, capacity) };
        if result != 1 {
            read.in_flight = false;
            drop(read);
            drop(gate);
            self.fail_then_cancel(Error::BidirectionalApi {
                operation: "read",
                code: result,
            });
        }
    }

    fn fail_then_cancel(&self, error: Error) {
        let mut pending = self
            .pending_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if pending.is_none() {
            *pending = Some(error);
        }
        drop(pending);
        self.control.cancel();
    }

    fn terminal(&self, fallback: Option<Error>) {
        if self.control.done.swap(true, Ordering::AcqRel) {
            return;
        }
        let error = self
            .pending_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .or(fallback);
        let result = error.clone().map_or(Ok(()), Err);
        if let Some(sender) = self
            .ready_sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = sender.send(result.clone());
        }
        if self.headers_sender.borrow().is_none() {
            self.headers_sender
                .send_replace(Some(result.clone().map(|()| {
                    BidirectionalResponseHeaders {
                        headers: Vec::new(),
                        negotiated_protocol: String::new(),
                    }
                })));
        }
        if self.trailers_sender.borrow().is_none() {
            self.trailers_sender
                .send_replace(Some(result.clone().map(|()| Vec::new())));
        }
        self.terminal_sender.send_replace(Some(result.clone()));
        self.write.finish(&result);
        let sender = self.read_sender.clone();
        self.handle.spawn(async move {
            let event = match result {
                Ok(()) => ReadEvent::Eof,
                Err(error) => ReadEvent::Error(error),
            };
            let _ = sender.send(event).await;
        });
        if let Some(sender) = self
            .cleanup_sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = sender.send(());
        }
    }
}

fn bidirectional_context(
    stream: *mut sys::bidirectional_stream,
) -> Option<BidirectionalCallbackLease> {
    if stream.is_null() {
        return None;
    }
    // SAFETY: callbacks receive our live stream whose annotation is raw Arc.
    let raw = unsafe { (*stream).annotation }.cast::<BidirectionalContext>();
    if raw.is_null() {
        None
    } else {
        // SAFETY: NativeBidirectionalStream retains the original strong count.
        unsafe {
            Arc::increment_strong_count(raw);
            let context = Arc::from_raw(raw);
            context.callback_count.fetch_add(1, Ordering::AcqRel);
            Some(BidirectionalCallbackLease(context))
        }
    }
}

struct BidirectionalCallbackLease(Arc<BidirectionalContext>);

impl std::ops::Deref for BidirectionalCallbackLease {
    type Target = BidirectionalContext;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for BidirectionalCallbackLease {
    fn drop(&mut self) {
        if self.0.callback_count.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.callbacks_idle.notify_waiters();
        }
    }
}

unsafe extern "C" fn on_stream_ready(stream: *mut sys::bidirectional_stream) {
    let Some(context) = bidirectional_context(stream) else {
        return;
    };
    context.write.ready.store(true, Ordering::Release);
    context.write.notify.notify_waiters();
    if let Some(sender) = context
        .ready_sender
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    {
        let _ = sender.send(Ok(()));
    }
}

unsafe extern "C" fn on_response_headers(
    stream: *mut sys::bidirectional_stream,
    headers: *const sys::bidirectional_stream_header_array,
    negotiated_protocol: *const c_char,
) {
    let Some(context) = bidirectional_context(stream) else {
        return;
    };
    // SAFETY: callback-owned headers and protocol are copied immediately.
    let headers = unsafe { copy_headers(headers) };
    let negotiated_protocol = if negotiated_protocol.is_null() {
        String::new()
    } else {
        // SAFETY: callback provides a NUL-terminated protocol string.
        unsafe { CStr::from_ptr(negotiated_protocol) }
            .to_string_lossy()
            .into_owned()
    };
    context
        .headers_sender
        .send_replace(Some(Ok(BidirectionalResponseHeaders {
            headers,
            negotiated_protocol,
        })));
    let task_context = context.0.clone();
    context
        .handle
        .spawn(async move { task_context.start_read() });
}

unsafe extern "C" fn on_read_completed(
    stream: *mut sys::bidirectional_stream,
    data: *mut c_char,
    bytes_read: i32,
) {
    let Some(context) = bidirectional_context(stream) else {
        return;
    };
    let mut read = context
        .read
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    read.in_flight = false;
    if bytes_read < 0 {
        drop(read);
        context.fail_then_cancel(Error::BidirectionalApi {
            operation: "read callback",
            code: bytes_read,
        });
        return;
    }
    let count = usize::try_from(bytes_read).expect("negative reads returned above");
    if count > read.buffer.len() || (count != 0 && data.is_null()) {
        drop(read);
        context.fail_then_cancel(Error::InvalidReadSize {
            reported: count as u64,
            capacity: context
                .read
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .buffer
                .len() as u64,
        });
        return;
    }
    if count == 0 {
        read.eof = true;
        drop(read);
        let sender = context.read_sender.clone();
        context.handle.spawn(async move {
            let _ = sender.send(ReadEvent::Eof).await;
        });
        return;
    }
    // SAFETY: callback reports count readable bytes in our supplied buffer.
    let chunk = Bytes::copy_from_slice(unsafe { std::slice::from_raw_parts(data.cast(), count) });
    drop(read);
    let sender = context.read_sender.clone();
    let task_context = context.0.clone();
    context.handle.spawn(async move {
        if sender.send(ReadEvent::Chunk(chunk)).await.is_err() {
            task_context.fail_then_cancel(Error::Canceled);
        } else {
            task_context.start_read();
        }
    });
}

unsafe extern "C" fn on_write_completed(
    stream: *mut sys::bidirectional_stream,
    data: *const c_char,
) {
    let Some(context) = bidirectional_context(stream) else {
        return;
    };
    context.write.complete(data as usize);
}

unsafe extern "C" fn on_response_trailers(
    stream: *mut sys::bidirectional_stream,
    trailers: *const sys::bidirectional_stream_header_array,
) {
    let Some(context) = bidirectional_context(stream) else {
        return;
    };
    // SAFETY: callback-owned headers are copied immediately.
    context
        .trailers_sender
        .send_replace(Some(Ok(unsafe { copy_headers(trailers) })));
}

unsafe extern "C" fn on_succeeded(stream: *mut sys::bidirectional_stream) {
    if let Some(context) = bidirectional_context(stream) {
        context.terminal(None);
    }
}

unsafe extern "C" fn on_failed(stream: *mut sys::bidirectional_stream, net_error: i32) {
    if let Some(context) = bidirectional_context(stream) {
        context.terminal(Some(Error::BidirectionalStream { net_error }));
    }
}

unsafe extern "C" fn on_canceled(stream: *mut sys::bidirectional_stream) {
    if let Some(context) = bidirectional_context(stream) {
        context.terminal(Some(Error::Canceled));
    }
}

unsafe fn copy_headers(raw: *const sys::bidirectional_stream_header_array) -> Vec<Header> {
    if raw.is_null() {
        return Vec::new();
    }
    // SAFETY: callback owns the array through this copy.
    let array = unsafe { &*raw };
    if array.headers.is_null() {
        return Vec::new();
    }
    let mut headers = Vec::with_capacity(array.count);
    for index in 0..array.count {
        // SAFETY: index is within array.count.
        let header = unsafe { &*array.headers.add(index) };
        // SAFETY: callback supplies NUL-terminated strings.
        let name = unsafe { copy_c_string(header.key) };
        // SAFETY: callback supplies NUL-terminated strings.
        let value = unsafe { copy_c_string(header.value) };
        if let Ok(header) = Header::new(name, value) {
            headers.push(header);
        }
    }
    headers
}

enum WriteCommand {
    Data { data: Bytes, end_of_stream: bool },
    Flush(oneshot::Sender<Result<()>>),
}

struct InFlightWrite {
    _data: Bytes,
    end_of_stream: bool,
}

struct WriteState {
    queue: VecDeque<WriteCommand>,
    in_flight: HashMap<usize, VecDeque<InFlightWrite>>,
    in_flight_count: usize,
    flush_waiters: Vec<oneshot::Sender<Result<()>>>,
    flush_blocked: bool,
    local_closed: bool,
    terminal: Option<Result<()>>,
}

struct WriteShared {
    state: Mutex<WriteState>,
    capacity: usize,
    ready: AtomicBool,
    notify: Notify,
    producer_waker: Mutex<Option<Waker>>,
}

impl WriteShared {
    fn new(capacity: usize) -> Self {
        Self {
            state: Mutex::new(WriteState {
                queue: VecDeque::new(),
                in_flight: HashMap::new(),
                in_flight_count: 0,
                flush_waiters: Vec::new(),
                flush_blocked: false,
                local_closed: false,
                terminal: None,
            }),
            capacity,
            ready: AtomicBool::new(false),
            notify: Notify::new(),
            producer_waker: Mutex::new(None),
        }
    }

    fn enqueue_data(&self, data: Bytes, end_of_stream: bool, waker: &Waker) -> Result<bool> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(Err(error)) = &state.terminal {
            return Err(error.clone());
        }
        if state.local_closed {
            return Err(Error::Canceled);
        }
        if state.queue_data_count() + state.in_flight_count >= self.capacity {
            drop(state);
            *self
                .producer_waker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(waker.clone());
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.queue_data_count() + state.in_flight_count >= self.capacity {
                return Ok(false);
            }
            drop(state);
            return self.enqueue_data(data, end_of_stream, waker);
        }
        if end_of_stream {
            state.local_closed = true;
        }
        state.queue.push_back(WriteCommand::Data {
            data,
            end_of_stream,
        });
        drop(state);
        self.notify.notify_one();
        Ok(true)
    }

    fn enqueue_control(&self, command: WriteCommand) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(Err(error)) = &state.terminal {
            return Err(error.clone());
        }
        state.queue.push_back(command);
        drop(state);
        self.notify.notify_one();
        Ok(())
    }

    fn complete(&self, pointer: usize) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (completed, remove_entry) =
            state
                .in_flight
                .get_mut(&pointer)
                .map_or((None, false), |writes| {
                    let completed = writes.pop_front();
                    (completed, writes.is_empty())
                });
        if let Some(write) = completed {
            state.in_flight_count = state.in_flight_count.saturating_sub(1);
            if write.end_of_stream {
                state.local_closed = true;
            }
        }
        if remove_entry {
            state.in_flight.remove(&pointer);
        }
        if state.in_flight_count == 0 && state.flush_blocked {
            state.flush_blocked = false;
            for waiter in state.flush_waiters.drain(..) {
                let _ = waiter.send(Ok(()));
            }
            self.notify.notify_one();
        }
        drop(state);
        self.wake_producer();
    }

    fn finish(&self, result: &Result<()>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.terminal = Some(result.clone());
        for waiter in state.flush_waiters.drain(..) {
            let _ = waiter.send(result.clone());
        }
        while let Some(command) = state.queue.pop_front() {
            if let WriteCommand::Flush(waiter) = command {
                let _ = waiter.send(result.clone());
            }
        }
        drop(state);
        self.wake_producer();
        self.notify.notify_waiters();
    }

    fn wake_producer(&self) {
        if let Some(waker) = self
            .producer_waker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            waker.wake();
        }
    }
}

impl WriteState {
    fn queue_data_count(&self) -> usize {
        self.queue
            .iter()
            .filter(|command| matches!(command, WriteCommand::Data { .. }))
            .count()
    }
}

async fn write_loop(control: Arc<BidirectionalControl>, write: Arc<WriteShared>) {
    loop {
        let notified = write.notify.notified();
        if !write.ready.load(Ordering::Acquire) {
            notified.await;
            continue;
        }
        let command = {
            let mut state = write
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.terminal.is_some() || state.flush_blocked {
                None
            } else {
                state.queue.pop_front()
            }
        };
        let Some(command) = command else {
            let terminal = write
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .terminal
                .is_some();
            if terminal {
                return;
            }
            notified.await;
            continue;
        };
        match command {
            WriteCommand::Data {
                data,
                end_of_stream,
            } => {
                let pointer = data.as_ptr() as usize;
                {
                    let mut state = write
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state
                        .in_flight
                        .entry(pointer)
                        .or_default()
                        .push_back(InFlightWrite {
                            _data: data.clone(),
                            end_of_stream,
                        });
                    state.in_flight_count += 1;
                }
                if let Err(error) = control.write(&data, end_of_stream) {
                    write.finish(&Err(error));
                    control.cancel();
                    return;
                }
            }
            WriteCommand::Flush(waiter) => {
                if let Err(error) = control.flush() {
                    let _ = waiter.send(Err(error.clone()));
                    write.finish(&Err(error));
                    control.cancel();
                    return;
                }
                let mut state = write
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.in_flight_count == 0 {
                    let _ = waiter.send(Ok(()));
                } else {
                    state.flush_blocked = true;
                    state.flush_waiters.push(waiter);
                }
            }
        }
    }
}

const fn priority_value(priority: Priority) -> i32 {
    match priority {
        Priority::Idle => 0,
        Priority::Lowest => 1,
        Priority::Low => 2,
        Priority::Medium => 3,
        Priority::Highest => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_tokio_stream<T>()
    where
        T: AsyncRead + AsyncWrite + Stream<Item = Result<Bytes>> + Send + Unpin,
    {
    }

    #[test]
    fn validates_builder() {
        assert_tokio_stream::<BidirectionalStream>();
        let request = BidirectionalRequest::builder("https://example.com/rpc")
            .unwrap()
            .header("content-type", "application/grpc")
            .unwrap()
            .disable_auto_flush(true)
            .build()
            .unwrap();
        assert_eq!(request.method, "POST");
        assert!(
            BidirectionalRequest::builder("https://example.com")
                .unwrap()
                .write_capacity(0)
                .build()
                .is_err()
        );
    }
}
