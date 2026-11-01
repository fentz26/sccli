//! Poll Windows socket reads beneath TLS so cancellation does not depend on
//! shutdown waking a pending Winsock receive. Preserve the original read budget.

use super::requests::Cancel;
use std::{
    io,
    time::{Duration, Instant},
};
use ureq::{
    Agent, Error,
    config::Config,
    unversioned::{
        resolver::DefaultResolver,
        transport::{
            Buffers, ConnectProxyConnector, ConnectionDetails, Connector, NextTimeout, RustlsConnector, TcpConnector,
            Transport,
        },
    },
};

pub fn agent(config: Config, cancel: Cancel) -> Agent {
    let connector = ConnectProxyConnector::default()
        .chain(TcpConnector::default())
        .chain(PollReads(cancel))
        .chain(RustlsConnector::default());
    Agent::with_parts(config, connector, DefaultResolver::default())
}

#[derive(Debug)]
struct PollReads(Cancel);

impl<T: Transport> Connector<T> for PollReads {
    type Out = PollTransport<T>;
    fn connect(&self, _: &ConnectionDetails, chained: Option<T>) -> Result<Option<Self::Out>, Error> {
        Ok(chained.map(|inner| PollTransport {
            inner,
            cancel: self.0.clone(),
        }))
    }
}

#[derive(Debug)]
struct PollTransport<T> {
    inner: T,
    cancel: Cancel,
}

impl<T: Transport> Transport for PollTransport<T> {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.inner.buffers()
    }
    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), Error> {
        self.inner.transmit_output(amount, timeout)
    }
    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, Error> {
        let budget = timeout.for_read();
        let started = Instant::now();
        loop {
            if self.cancel.is_stale() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "request cancelled").into());
            }
            let left = budget.after.saturating_sub(started.elapsed());
            if left.is_zero() {
                return Err(Error::Timeout(budget.reason));
            }
            let slice = left.min(Duration::from_millis(100));
            let next = NextTimeout {
                after: slice.into(),
                per_read: None,
                ..timeout
            };
            match self.inner.await_input(next) {
                Err(Error::Timeout(_)) if left > slice => continue,
                result => return result,
            }
        }
    }
    fn is_open(&mut self) -> bool {
        self.inner.is_open()
    }
    fn is_tls(&self) -> bool {
        self.inner.is_tls()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{Arc, atomic::AtomicU64},
        thread,
    };

    #[test]
    fn polling_reads_interrupt_a_waiting_response() {
        use std::sync::{atomic::Ordering, mpsc};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/slow", listener.local_addr().unwrap());
        let epoch = Arc::new(AtomicU64::new(0));
        let agent = agent(
            Config::builder()
                .proxy(None)
                .timeout_global(Some(Duration::from_secs(3)))
                .build(),
            Cancel::tied_to(&epoch),
        );
        thread::scope(|scope| {
            let (ready_tx, ready_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            scope.spawn(move || {
                let mut socket = crate::api::tests::accept_test_connection(&listener);
                socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut request = [0; 4096];
                assert!(socket.read(&mut request).unwrap() > 0);
                ready_tx.send(()).unwrap();
                let _ = release_rx.recv_timeout(Duration::from_secs(5));
            });
            let (result_tx, result_rx) = mpsc::channel();
            scope.spawn(move || {
                let _ = result_tx.send(agent.get(&url).call().is_err());
            });
            ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            epoch.fetch_add(1, Ordering::Relaxed);
            let result = result_rx.recv_timeout(Duration::from_secs(1));
            let _ = release_tx.send(());
            assert!(result.unwrap());
        });
    }

    #[test]
    fn polling_reads_allow_a_slow_response_without_cancellation() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/slow", listener.local_addr().unwrap());
        let epoch = Arc::new(AtomicU64::new(0));
        let agent = agent(
            Config::builder()
                .proxy(None)
                .timeout_global(Some(Duration::from_secs(3)))
                .build(),
            Cancel::tied_to(&epoch),
        );
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut socket = crate::api::tests::accept_test_connection(&listener);
                socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut request = [0; 4096];
                assert!(socket.read(&mut request).unwrap() > 0);
                thread::sleep(Duration::from_millis(350));
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                    .unwrap();
            });
            assert_eq!(
                agent.get(&url).call().unwrap().body_mut().read_to_string().unwrap(),
                "{}"
            );
        });
    }
}
