//! The connection to mpv: a Unix socket on Linux and macOS, a named pipe on Windows.

#[cfg(unix)]
mod imp {
    use std::{io, os::unix::net::UnixStream};

    pub type Conn = UnixStream;
    pub type Reader = UnixStream;

    pub fn address() -> String {
        let name = format!("scc-{}.sock", std::process::id());
        std::env::temp_dir().join(name).display().to_string()
    }

    pub fn connect(addr: &str) -> io::Result<Conn> {
        UnixStream::connect(addr)
    }

    pub fn reader(c: &Conn) -> io::Result<Reader> {
        c.try_clone()
    }

    pub fn cleanup(addr: &str) {
        let _ = std::fs::remove_file(addr);
    }
}

#[cfg(windows)]
mod imp {
    use std::{
        fs::{File, OpenOptions},
        io::{self, Read},
        os::windows::io::AsRawHandle,
        ptr::null_mut,
        thread,
        time::Duration,
    };
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;

    pub type Conn = File;

    /// A blocking read on a synchronous pipe handle also blocks writes to that pipe, so a
    /// pause command could wait until mpv next said something. This reader only reads once
    /// data is waiting, which leaves the pipe free for writes.
    pub struct Reader(File);

    impl Read for Reader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            loop {
                let mut waiting = 0u32;
                // SAFETY: the handle stays open for the life of `self.0`, and a null buffer
                // with size 0 asks only for the byte count, written to a live local.
                let ok = unsafe {
                    PeekNamedPipe(
                        self.0.as_raw_handle() as _,
                        null_mut(),
                        0,
                        null_mut(),
                        &mut waiting,
                        null_mut(),
                    )
                };
                if ok == 0 {
                    return Err(io::Error::last_os_error());
                }
                if waiting > 0 {
                    let n = buf.len().min(waiting as usize);
                    return self.0.read(&mut buf[..n]);
                }
                thread::sleep(Duration::from_millis(15));
            }
        }
    }

    pub fn address() -> String {
        format!(r"\\.\pipe\scc-{}", std::process::id())
    }

    pub fn connect(addr: &str) -> io::Result<Conn> {
        OpenOptions::new().read(true).write(true).open(addr)
    }

    pub fn reader(c: &Conn) -> io::Result<Reader> {
        Ok(Reader(c.try_clone()?))
    }

    /// The pipe disappears with mpv.
    pub fn cleanup(_addr: &str) {}
}

pub use imp::*;
