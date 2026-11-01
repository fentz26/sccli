//! mpv process and its IPC socket. Startup never blocks the UI: commands sent before the
//! socket is ready wait in `pending`.

use crate::{app::Ev, config::Settings, ipc};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    sync::mpsc::Sender,
    thread,
    time::{Duration, Instant},
};

/// How to get a missing program on this system.
pub fn install_hint(tool: &str) -> String {
    if cfg!(windows) {
        format!("scoop install {tool}")
    } else if cfg!(target_os = "macos") {
        format!("brew install {tool}")
    } else {
        format!("install the {tool} package, e.g. sudo pacman -S {tool} or sudo apt install {tool}")
    }
}

pub struct Mpv {
    child: Child,
    conn: Option<ipc::Conn>,
    pending: Vec<Value>,
    addr: String,
}

impl Mpv {
    pub fn start(s: &Settings, tx: Sender<Ev>) -> Result<Mpv> {
        let addr = ipc::address();
        let (ahead, back, secs) = s.memory.mpv();
        let child = Command::new("mpv")
            .args([
                "--no-video",
                "--no-terminal",
                "--idle=yes",
                "--osc=no",
                "--audio-display=no",
                "--ytdl=no", // scc hands mpv direct URLs
                // Skipping mpv's scripts cut first sound from about 1.0s to 0.65s.
                "--load-scripts=no",
                "--load-stats-overlay=no",
                "--load-console=no",
                "--load-auto-profiles=no",
                "--cache=yes",
            ])
            .arg(format!("--cache-secs={secs}"))
            .arg(format!("--demuxer-max-bytes={ahead}"))
            .arg(format!("--demuxer-max-back-bytes={back}"))
            .arg(format!("--volume={}", s.volume))
            .arg(format!("--input-ipc-server={addr}"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("failed to run mpv ({})", install_hint("mpv")))?;

        let path = addr.clone();
        thread::spawn(move || {
            let t0 = Instant::now();
            let conn = loop {
                match ipc::connect(&path) {
                    Ok(s) => break s,
                    Err(_) if t0.elapsed() < Duration::from_secs(10) => thread::sleep(Duration::from_millis(20)),
                    Err(e) => {
                        let _ = tx.send(Ev::MpvFailed(format!("could not connect to mpv: {e}")));
                        return;
                    }
                }
            };
            let Ok(reader) = ipc::reader(&conn) else { return };
            let _ = tx.send(Ev::MpvReady(conn));
            for line in BufReader::new(reader).lines().map_while(Result::ok) {
                if let Ok(v) = serde_json::from_str::<Value>(&line)
                    && tx.send(Ev::Mpv(v)).is_err()
                {
                    break;
                }
            }
        });
        Ok(Mpv {
            child,
            conn: None,
            pending: vec![],
            addr,
        })
    }

    pub fn ready(&mut self, conn: ipc::Conn) {
        self.conn = Some(conn);
        for (i, p) in ["pause", "time-pos", "duration", "volume"].iter().enumerate() {
            self.send(json!(["observe_property", i + 1, p]));
        }
        for c in std::mem::take(&mut self.pending) {
            self.send(c);
        }
    }

    pub fn send(&mut self, command: Value) {
        match &mut self.conn {
            Some(s) => {
                let _ = writeln!(s, "{}", json!({ "command": command }));
            }
            None => self.pending.push(command),
        }
    }

    pub fn exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Mpv {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        ipc::cleanup(&self.addr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// A command must reach mpv while the reader is idle. On Windows this fails if a blocking
    /// read holds the pipe, which is the bug `ipc::Reader` exists to prevent.
    #[test]
    #[ignore = "needs mpv installed"]
    fn commands_reach_mpv_while_the_reader_is_idle() {
        let (tx, rx) = mpsc::channel();
        let mut mpv = Mpv::start(&Settings::default(), tx).unwrap();
        let timeout = Duration::from_secs(30);
        match rx.recv_timeout(timeout) {
            Ok(Ev::MpvReady(conn)) => mpv.ready(conn),
            _ => panic!("mpv did not connect"),
        }
        thread::sleep(Duration::from_secs(1)); // let the startup messages drain

        mpv.send(json!(["set_property", "volume", 37]));

        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Ok(Ev::Mpv(v)) = rx.recv_timeout(Duration::from_secs(1))
                && v["name"] == "volume"
                && v["data"] == 37.0
            {
                return;
            }
        }
        panic!("mpv never applied the command");
    }
}
