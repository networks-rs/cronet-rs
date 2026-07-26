use std::ffi::c_void;

use tokio::runtime::Handle;
use tokio_cronet_sys as sys;

use crate::{Error, Result};

/// Cronet executor whose runnables are scheduled directly onto the Tokio
/// runtime that created the engine.
pub(crate) struct Executor {
    raw: sys::Cronet_ExecutorPtr,
    context: *mut ExecutorContext,
}

struct ExecutorContext {
    runtime: Handle,
}

impl Executor {
    pub(crate) fn new() -> Result<Self> {
        let runtime = Handle::try_current().map_err(|_| Error::TokioRuntimeRequired)?;
        let context = Box::into_raw(Box::new(ExecutorContext { runtime }));
        // SAFETY: callback has the exact ABI expected by Cronet.
        let raw = unsafe { sys::Cronet_Executor_CreateWith(Some(execute)) };
        if raw.is_null() {
            // SAFETY: construction failed before Cronet retained the context.
            unsafe { drop(Box::from_raw(context)) };
            return Err(Error::AllocationFailed("Tokio callback executor"));
        }
        // SAFETY: both pointers remain valid until this object's Drop.
        unsafe { sys::Cronet_Executor_SetClientContext(raw, context.cast::<c_void>()) };
        Ok(Self { raw, context })
    }

    pub(crate) fn as_ptr(&self) -> sys::Cronet_ExecutorPtr {
        self.raw
    }

    pub(crate) fn handle(&self) -> &Handle {
        // SAFETY: context is created with the executor and destroyed with it.
        &unsafe { &*self.context }.runtime
    }
}

// SAFETY: all mutable state inside a Tokio Handle is synchronized. The native
// executor is only destroyed after Cronet shutdown has stopped submitting work.
unsafe impl Send for Executor {}
// SAFETY: Execute only reads the Handle and Handle::spawn is thread-safe.
unsafe impl Sync for Executor {}

impl Drop for Executor {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            // SAFETY: the engine has shut down, so no later Execute call exists.
            unsafe { sys::Cronet_Executor_Destroy(self.raw) };
            self.raw = std::ptr::null_mut();
        }
        if !self.context.is_null() {
            // SAFETY: reverses the single Box::into_raw in `new`.
            unsafe { drop(Box::from_raw(self.context)) };
            self.context = std::ptr::null_mut();
        }
    }
}

struct Runnable {
    raw: sys::Cronet_RunnablePtr,
}

// SAFETY: Cronet transfers ownership of a runnable to Executor::Execute and
// explicitly permits the configured executor to run it on another thread.
unsafe impl Send for Runnable {}

impl Runnable {
    fn run(mut self) {
        // SAFETY: this object uniquely owns the runnable supplied by Cronet.
        unsafe {
            sys::Cronet_Runnable_Run(self.raw);
            sys::Cronet_Runnable_Destroy(self.raw);
        }
        self.raw = std::ptr::null_mut();
    }
}

impl Drop for Runnable {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            // SAFETY: a task canceled before polling still owns the runnable.
            unsafe { sys::Cronet_Runnable_Destroy(self.raw) };
        }
    }
}

unsafe extern "C" fn execute(executor: sys::Cronet_ExecutorPtr, runnable: sys::Cronet_RunnablePtr) {
    if runnable.is_null() {
        return;
    }
    // SAFETY: Cronet invokes this with the live executor created above.
    let context =
        unsafe { sys::Cronet_Executor_GetClientContext(executor) }.cast::<ExecutorContext>();
    if context.is_null() {
        // SAFETY: the callback still owns a runnable it cannot schedule.
        unsafe { sys::Cronet_Runnable_Destroy(runnable) };
        return;
    }
    let runnable = Runnable { raw: runnable };
    // SAFETY: context remains alive until engine shutdown, which waits for
    // Cronet to stop invoking this callback.
    unsafe { &*context }
        .runtime
        .spawn(async move { runnable.run() });
}
