//! The TCP relay between OBS and libavformat's RTMP listener. It exists because
//! libavformat can't tell a deliberate "stop streaming" from a crash or a dropped
//! connection, and nicedelay ends the stream differently for each.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::thread;
use std::time::Duration;

use memchr::memmem;

// When OBS stops streaming on purpose, it sends the RTMP commands "FCUnpublish" and
// "deleteStream" before closing the connection; a crash or network drop just closes it.
// Commands are AMF0 strings on the wire: 0x02, a 16-bit length, then the name.
fn markers() -> [Vec<u8>; 2] {
    ["FCUnpublish", "deleteStream"].map(|name| {
        let mut m = vec![0x02, 0x00, name.len() as u8];
        m.extend_from_slice(name.as_bytes());
        m
    })
}

/// Watches the bytes OBS sends for its "stop streaming" commands.
pub struct StopDetector {
    markers: [Vec<u8>; 2],
    keep: usize,
    tail: Vec<u8>,
    pub seen: bool,
}

impl Default for StopDetector {
    fn default() -> Self {
        let markers = markers();
        let keep = markers.iter().map(Vec::len).max().unwrap_or(0) - 1;
        StopDetector { markers, keep, tail: vec![], seen: false }
    }
}

impl StopDetector {
    /// Feed the next chunk of the stream; returns true once a stop command was seen.
    pub fn push(&mut self, chunk: &[u8]) -> bool {
        if self.seen {
            return true;
        }
        // Keep the end of the previous chunk, so a command split across chunks is still found.
        self.tail.extend_from_slice(chunk);
        self.seen = self.markers.iter().any(|m| memmem::find(&self.tail, m).is_some());
        let cut = self.tail.len().saturating_sub(self.keep);
        self.tail.drain(..cut);
        self.seen
    }
}

/// A currently free TCP port on 127.0.0.1.
pub fn free_port() -> io::Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// Accepts OBS on the public address and forwards each connection to the internal port
/// libavformat currently listens on. One publisher at a time.
pub struct Relay {
    pub internal_port: Arc<AtomicU16>,
    /// OBS sent its "stop streaming" commands on the current or last connection.
    pub ended_by_obs: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    local: std::net::SocketAddr,
}

impl Relay {
    pub fn start(host: &str, port: u16) -> io::Result<Relay> {
        let listener = TcpListener::bind((host, port))?;
        let local = listener.local_addr()?;
        let relay = Relay {
            internal_port: Arc::new(AtomicU16::new(0)),
            ended_by_obs: Arc::new(AtomicBool::new(false)),
            stopped: Arc::new(AtomicBool::new(false)),
            local,
        };
        let (internal, ended, stopped) =
            (relay.internal_port.clone(), relay.ended_by_obs.clone(), relay.stopped.clone());
        let busy = Arc::new(AtomicBool::new(false));
        thread::Builder::new().name("relay".into()).spawn(move || {
            for obs in listener.incoming() {
                if stopped.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(obs) = obs else { continue };
                if busy.swap(true, Ordering::AcqRel) {
                    let _ = obs.shutdown(Shutdown::Both); // one publisher at a time
                    continue;
                }
                ended.store(false, Ordering::Relaxed);
                let (internal, ended, busy) = (internal.clone(), ended.clone(), busy.clone());
                thread::spawn(move || {
                    relay_one(obs, &internal, &ended);
                    busy.store(false, Ordering::Release);
                });
            }
        })?;
        Ok(relay)
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        // Wake the accept loop so it sees the flag.
        let _ = TcpStream::connect_timeout(&self.local, Duration::from_millis(200));
    }
}

/// Relays one OBS connection to the waiting libavformat listener until either side closes.
fn relay_one(obs: TcpStream, internal: &AtomicU16, ended: &AtomicBool) {
    let _ = obs.set_nodelay(true);
    // libavformat may still be getting ready to listen (it restarts after each connection).
    let mut upstream = None;
    for _ in 0..50 {
        let port = internal.load(Ordering::Relaxed);
        if port != 0
            && let Ok(s) = TcpStream::connect(("127.0.0.1", port))
        {
            upstream = Some(s);
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let Some(up) = upstream else {
        let _ = obs.shutdown(Shutdown::Both);
        return;
    };
    let _ = up.set_nodelay(true);
    let (Ok(mut obs_r), Ok(mut up_w)) = (obs.try_clone(), up.try_clone()) else { return };
    let down = thread::spawn(move || {
        let (mut up_r, mut obs_w) = (up, obs);
        let _ = io::copy(&mut up_r, &mut obs_w);
        let _ = obs_w.shutdown(Shutdown::Both);
    });
    let mut detector = StopDetector::default();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match obs_r.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if detector.push(&buf[..n]) {
                    ended.store(true, Ordering::Relaxed);
                }
                if up_w.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
        }
    }
    let _ = up_w.shutdown(Shutdown::Both);
    let _ = down.join();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An RTMP command name as it appears on the wire (AMF0 string).
    fn amf(name: &str) -> Vec<u8> {
        let mut v = vec![0x02, 0x00, name.len() as u8];
        v.extend_from_slice(name.as_bytes());
        v
    }

    #[test]
    fn spots_the_stop_command() {
        let mut d = StopDetector::default();
        assert!(!d.push(&[7; 5000]));
        assert!(d.push(&[vec![0; 100], amf("deleteStream"), vec![0; 10]].concat()));
    }

    #[test]
    fn spots_a_command_split_across_chunks() {
        let mut d = StopDetector::default();
        let cmd = amf("FCUnpublish");
        assert!(!d.push(&[vec![0; 300], cmd[..6].to_vec()].concat()));
        assert!(d.push(&[cmd[6..].to_vec(), vec![0; 300]].concat()));
    }

    #[test]
    fn ignores_the_start_commands() {
        let mut d = StopDetector::default();
        assert!(!d.push(&[amf("FCPublish"), amf("createStream"), amf("publish")].concat()));
    }
}
