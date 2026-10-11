//! Shared, absolute read budget for the loopback hook/MCP receiver.
use std::{io::{self, Read}, net::TcpStream, time::{Duration, Instant}};

pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
pub(crate) struct Deadline(Instant);

impl Deadline {
    pub(crate) fn new(timeout: Duration) -> Self {
        Self(Instant::now() + timeout)
    }

    pub(crate) fn arm(&self, stream: &TcpStream) -> Result<(), &'static str> {
        let remaining = self.0.checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero()).ok_or("408 Request Timeout")?;
        stream.set_read_timeout(Some(remaining)).map_err(|_| "400 Bad Request")
    }

    pub(crate) fn read(&self, stream: &mut TcpStream, buf: &mut [u8]) -> Result<usize, &'static str> {
        self.arm(stream)?;
        stream.read(buf).map_err(|e| match e.kind() {
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => "408 Request Timeout",
            _ => "400 Bad Request",
        })
    }
}

pub(crate) fn read_head(stream: &mut TcpStream, deadline: Deadline) -> Result<(Vec<u8>, usize), &'static str> {
    let mut buf = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        let n = deadline.read(stream, &mut chunk)?;
        if n == 0 { return Err("400 Bad Request"); }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            if p + 4 > 64 * 1024 { return Err("431 Request Header Fields Too Large"); }
            return Ok((buf, p + 4));
        }
        if buf.len() > 64 * 1024 { return Err("431 Request Header Fields Too Large"); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, net::TcpListener};

    #[test]
    fn trickled_headers_do_not_reset_the_total_read_budget() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        let deadline = Deadline::new(Duration::from_millis(200));
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            tx.send(read_head(&mut server, deadline)).unwrap();
        });
        client.write_all(b"POST /hook HTTP/1.1\r\nX: ").unwrap();
        let mut result = None;
        for _ in 0..20 {
            if let Ok(done) = rx.recv_timeout(Duration::from_millis(50)) { result = Some(done); break; }
            if client.write_all(b"x").is_err() { break; }
        }
        drop(client);
        reader.join().unwrap();
        let result = result.unwrap_or_else(|| rx.recv().unwrap());
        assert!(matches!(result, Err("408 Request Timeout")), "{result:?}");
    }
}
