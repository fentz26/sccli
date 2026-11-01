//! Windows may leave a synchronous socket read pending after shutdown. Cancel the
//! current request thread's pending I/O too, without affecting other requests.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows_sys::Win32::System::{
    IO::CancelSynchronousIo,
    Threading::{GetCurrentThreadId, OpenThread, THREAD_TERMINATE},
};

pub struct CancelIo(OwnedHandle);

impl CancelIo {
    pub fn current() -> Option<Self> {
        // Obtain a real handle, rather than the current-thread pseudo handle:
        // the watcher must refer to this request thread when it runs elsewhere.
        let handle = unsafe { OpenThread(THREAD_TERMINATE, 0, GetCurrentThreadId()) };
        if handle.is_null() {
            return None;
        }
        // OpenThread returns a new owned handle, closed when the watcher stops.
        Some(Self(unsafe { OwnedHandle::from_raw_handle(handle) }))
    }

    pub fn cancel(&self) {
        // No pending I/O is a normal race; shutdown still prevents subsequent I/O.
        let _ = unsafe { CancelSynchronousIo(self.0.as_raw_handle()) };
    }
}
