//! The rolling recording: compressed packets exactly as OBS sent them, in chunk files on
//! disk (one directory per OBS connection), indexed in memory, plus small thumbnails used
//! for the on-stream scrub animation.
//!
//! Times are "media seconds" on one global timeline: each ingest session starts where the
//! previous one ended, so disconnect gaps collapse instead of leaving holes.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use ffmpeg_next::Rational;
use ffmpeg_next::codec::Parameters;

use crate::media::picture::Picture;

/// Chunk files are closed and a new one started at this size.
const CHUNK_BYTES: u64 = 16 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Video,
    Audio,
}

#[derive(Clone, Copy, Debug)]
pub struct PacketMeta {
    pub kind: Kind,
    /// timestamps in the stream's time base, as received
    pub pts: i64,
    pub dts: i64,
    pub duration: i64,
    pub key: bool,
    /// session-local seconds of `pts`
    pub time: f64,
    chunk: u64,
    offset: u64,
    size: u32,
}

/// What the decoders of a session need to know about its streams.
#[derive(Clone)]
pub struct Streams {
    pub video: Parameters,
    pub video_tb: Rational,
    pub audio: Option<(Parameters, Rational)>,
}

// The codec parameters are written once, when the session is created, and only read
// (copied into decoders) afterwards.
unsafe impl Sync for Streams {}

pub struct Session {
    pub id: u64,
    pub dir: PathBuf,
    pub streams: Streams,
    state: Mutex<SessionState>,
    cond: Condvar,
}

#[derive(Default)]
struct SessionState {
    /// global time at which this session starts
    base: f64,
    registered: bool,
    packets: VecDeque<PacketMeta>,
    /// absolute index of packets[0]
    first: u64,
    /// absolute index and session-local time of each video keyframe
    keyframes: VecDeque<(u64, f64)>,
    /// chunk id and the absolute index of its last packet
    chunks: VecDeque<(u64, u64)>,
    writer: Option<(u64, File, u64)>,
    next_chunk: u64,
    /// session-local media seconds received so far
    live_seconds: f64,
    ended: bool,
}

/// What a reader finds at a packet index.
pub enum Next {
    Packet(PacketMeta),
    /// not received yet
    Wait,
    /// trimmed away, or the session ended before it
    Gone,
}

impl Session {
    fn lock(&self) -> MutexGuard<'_, SessionState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn base(&self) -> f64 {
        self.lock().base
    }

    /// Global time of the newest media in this session.
    pub fn end(&self) -> f64 {
        let s = self.lock();
        s.base + s.live_seconds
    }

    fn chunk_path(&self, id: u64) -> PathBuf {
        self.dir.join(format!("chunk-{id:08}.bin"))
    }

    /// Appends a packet: its bytes go to the current chunk file, its metadata to the index.
    pub fn append(&self, mut meta: PacketMeta, data: &[u8]) -> io::Result<()> {
        let mut s = self.lock();
        if s.writer.as_ref().is_none_or(|(_, _, size)| *size >= CHUNK_BYTES) {
            let id = s.next_chunk;
            s.next_chunk += 1;
            let file = OpenOptions::new().create(true).append(true).open(self.chunk_path(id))?;
            s.writer = Some((id, file, 0));
            s.chunks.push_back((id, 0));
        }
        let (id, file, size) = s.writer.as_mut().expect("writer opened above");
        file.write_all(data)?;
        meta.chunk = *id;
        meta.offset = *size;
        meta.size = data.len() as u32;
        *size += data.len() as u64;
        let index = s.first + s.packets.len() as u64;
        if let Some(last) = s.chunks.back_mut() {
            last.1 = index;
        }
        if meta.kind == Kind::Video && meta.key {
            s.keyframes.push_back((index, meta.time));
        }
        s.packets.push_back(meta);
        s.live_seconds = s.live_seconds.max(meta.time);
        drop(s);
        self.cond.notify_all();
        Ok(())
    }

    /// Advances the live edge to session-local time `t` (media received so far).
    pub fn advance(&self, t: f64) {
        let mut s = self.lock();
        s.live_seconds = s.live_seconds.max(t);
    }

    pub fn finish(&self) {
        let mut s = self.lock();
        s.ended = true;
        s.writer = None;
        drop(s);
        self.cond.notify_all();
    }

    #[cfg(test)]
    pub fn packet(&self, index: u64) -> Next {
        let s = self.lock();
        Self::lookup(&s, index)
    }

    fn lookup(s: &SessionState, index: u64) -> Next {
        if index < s.first {
            return Next::Gone;
        }
        match s.packets.get((index - s.first) as usize) {
            Some(p) => Next::Packet(*p),
            None if s.ended => Next::Gone,
            None => Next::Wait,
        }
    }

    /// Like [`Session::packet`], but waits up to `timeout` for a packet not received yet.
    pub fn wait_packet(&self, index: u64, timeout: Duration) -> Next {
        let s = self.lock();
        if let Next::Wait = Self::lookup(&s, index) {
            let (s, _) = self.cond.wait_timeout(s, timeout).unwrap_or_else(|e| e.into_inner());
            return Self::lookup(&s, index);
        }
        Self::lookup(&s, index)
    }

    /// Wakes readers waiting for packets (e.g. so they notice they were destroyed).
    pub fn wake(&self) {
        self.cond.notify_all();
    }

    /// Where a reader must start to play session-local time `local`: the latest keyframe at
    /// or before it (or the first one), moved back over audio packets that arrived just
    /// before the keyframe but belong to the same moment. Also returns the keyframe's time.
    fn locate(&self, local: f64) -> Option<(u64, f64)> {
        let s = self.lock();
        let k = s.keyframes.partition_point(|&(_, t)| t <= local);
        let Some(&(mut index, time)) = s.keyframes.get(k.saturating_sub(1)) else {
            // Nothing decodable yet: start with whatever arrives next.
            return (!s.ended).then(|| (s.first + s.packets.len() as u64, local));
        };
        while index > s.first {
            let p = &s.packets[(index - 1 - s.first) as usize];
            if p.kind != Kind::Audio || p.time < time - 0.25 {
                break;
            }
            index -= 1;
        }
        Some((index, time))
    }
}

/// Reads packet bytes back from a session's chunk files.
#[derive(Default)]
pub struct ChunkReader {
    open: Option<(PathBuf, File)>,
}

impl ChunkReader {
    pub fn read(&mut self, session: &Session, meta: &PacketMeta) -> io::Result<Vec<u8>> {
        let path = session.chunk_path(meta.chunk);
        if self.open.as_ref().is_none_or(|(p, _)| *p != path) {
            self.open = Some((path.clone(), File::open(&path)?));
        }
        let (_, file) = self.open.as_ref().expect("opened above");
        let mut buf = vec![0u8; meta.size as usize];
        read_exact_at(file, &mut buf, meta.offset)?;
        Ok(buf)
    }
}

#[cfg(unix)]
fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

#[cfg(windows)]
fn read_exact_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, offset)? {
            0 => return Err(io::ErrorKind::UnexpectedEof.into()),
            n => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
        }
    }
    Ok(())
}

#[derive(Clone)]
pub struct Thumb {
    pub t: f64,
    pub pic: Picture,
}

/// Where a reader starts: a keyframe packet of a session, and the global time of the first
/// frame it outputs.
pub struct Location {
    pub session: Arc<Session>,
    pub index: u64,
    pub start: f64,
}

pub struct Store {
    pub root: PathBuf,
    inner: Mutex<Inner>,
}

struct Inner {
    sessions: Vec<Arc<Session>>,
    thumbs: VecDeque<Thumb>,
    buffer_seconds: f64,
    next_id: u64,
}

impl Store {
    pub fn new(buffer_seconds: f64, root: Option<PathBuf>) -> io::Result<Store> {
        let root =
            root.unwrap_or_else(|| std::env::temp_dir().join(format!("nicedelay-buffer-{}", std::process::id())));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root)?;
        Ok(Store {
            root,
            inner: Mutex::new(Inner { sessions: vec![], thumbs: VecDeque::new(), buffer_seconds, next_id: 1 }),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_buffer_seconds(&self, s: f64) {
        self.lock().buffer_seconds = s;
    }

    /// Sessions are created when OBS connects, and join the timeline (register) once its
    /// first packet arrives.
    pub fn create_session(&self, streams: Streams) -> io::Result<Arc<Session>> {
        let id = {
            let mut inner = self.lock();
            inner.next_id += 1;
            inner.next_id - 1
        };
        let dir = self.root.join(format!("session-{id}"));
        fs::create_dir_all(&dir)?;
        Ok(Arc::new(Session { id, dir, streams, state: Mutex::default(), cond: Condvar::new() }))
    }

    pub fn register(&self, session: &Arc<Session>) {
        let mut inner = self.lock();
        let base = inner.sessions.last().map_or(0.0, |s| s.end());
        let mut s = session.lock();
        s.base = base;
        s.registered = true;
        drop(s);
        inner.sessions.push(session.clone());
    }

    pub fn discard(&self, session: &Session) {
        if !session.lock().registered {
            let _ = fs::remove_dir_all(&session.dir);
        }
    }

    /// Live edge on the global timeline.
    pub fn live_edge(&self) -> f64 {
        self.lock().sessions.last().map_or(0.0, |s| s.end())
    }

    pub fn oldest(&self) -> f64 {
        let sessions = self.lock().sessions.clone();
        for s in &sessions {
            let st = s.lock();
            if let Some(&(_, t)) = st.keyframes.front() {
                return st.base + t;
            }
            if !st.ended {
                return st.base;
            }
        }
        self.live_edge()
    }

    pub fn recorded(&self) -> f64 {
        (self.live_edge() - self.oldest()).max(0.0)
    }

    /// Where a reader must start to play global time `t`.
    pub fn locate(&self, t: f64) -> Option<Location> {
        let sessions = self.lock().sessions.clone();
        for (i, s) in sessions.iter().enumerate().rev() {
            let base = s.base();
            if t < base && i > 0 {
                continue;
            }
            let local = (t - base).max(0.0);
            if let Some((index, key_time)) = s.locate(local) {
                return Some(Location { session: s.clone(), index, start: base + local.max(key_time) });
            }
        }
        None
    }

    pub fn next_session(&self, session: &Session) -> Option<Arc<Session>> {
        let inner = self.lock();
        let i = inner.sessions.iter().position(|s| s.id == session.id)?;
        inner.sessions.get(i + 1).cloned()
    }

    pub fn add_thumb(&self, t: f64, pic: Picture) {
        self.lock().thumbs.push_back(Thumb { t, pic });
    }

    /// Nearest thumbnail to global time `t`.
    pub fn thumb_at(&self, t: f64) -> Option<Thumb> {
        let inner = self.lock();
        let th = &inner.thumbs;
        if th.is_empty() {
            return None;
        }
        let mut i = th.partition_point(|x| x.t < t).min(th.len() - 1);
        if i > 0 && (th[i - 1].t - t).abs() < (th[i].t - t).abs() {
            i -= 1;
        }
        Some(th[i].clone())
    }

    /// Drop everything older than the buffer window, but never what a reader still needs.
    pub fn trim(&self, protect_from: f64) {
        let live_edge = self.live_edge();
        let mut inner = self.lock();
        let cutoff = f64::min(live_edge - inner.buffer_seconds - 5.0, protect_from - 5.0);
        while inner.thumbs.front().is_some_and(|t| t.t < cutoff) {
            inner.thumbs.pop_front();
        }
        for session in &inner.sessions {
            let mut s = session.lock();
            // Cut at a keyframe, so what's left stays decodable.
            let local = cutoff - s.base;
            let k = s.keyframes.partition_point(|&(_, t)| t <= local);
            let keep_from = if k == s.keyframes.len() && s.ended {
                s.first + s.packets.len() as u64 // all of it is older
            } else if k > 0 {
                s.keyframes[k - 1].0
            } else {
                continue;
            };
            while s.first < keep_from && !s.packets.is_empty() {
                s.packets.pop_front();
                s.first += 1;
            }
            while s.keyframes.front().is_some_and(|&(i, _)| i < s.first) {
                s.keyframes.pop_front();
            }
            let writing = s.writer.as_ref().map(|w| w.0);
            while let Some(&(id, last)) = s.chunks.front() {
                if last >= s.first || Some(id) == writing {
                    break;
                }
                s.chunks.pop_front();
                let _ = fs::remove_file(session.chunk_path(id));
            }
        }
        while inner.sessions.len() > 1 {
            let first = &inner.sessions[0];
            let empty = {
                let s = first.lock();
                s.ended && s.packets.is_empty()
            };
            if !empty {
                break;
            }
            let s = inner.sessions.remove(0);
            let _ = fs::remove_dir_all(&s.dir);
        }
    }

    pub fn dispose(&self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl PacketMeta {
    pub fn new(kind: Kind, pts: i64, dts: i64, duration: i64, key: bool, time: f64) -> Self {
        PacketMeta { kind, pts, dts, duration, key, time, chunk: 0, offset: 0, size: 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        let root = std::env::temp_dir().join(format!(
            "nicedelay-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        Store::new(60.0, Some(root)).unwrap()
    }

    fn streams() -> Streams {
        Streams { video: Parameters::new(), video_tb: Rational(1, 1000), audio: None }
    }

    /// Video packets every 0.5 s from `from` to `to`, a keyframe every 2 s.
    fn record(s: &Session, from: f64, to: f64) {
        let mut t = from;
        while t < to {
            let key = (t % 2.0).abs() < 1e-9;
            let ms = (t * 1000.0) as i64;
            s.append(PacketMeta::new(Kind::Video, ms, ms, 500, key, t), &[t as u8; 10]).unwrap();
            t += 0.5;
        }
        s.advance(to);
    }

    #[test]
    fn finds_the_keyframe_at_or_before_a_time() {
        let st = store();
        let s = st.create_session(streams()).unwrap();
        st.register(&s);
        record(&s, 0.0, 5.0);
        let loc = st.locate(3.5).unwrap();
        let Next::Packet(p) = s.packet(loc.index) else { panic!("missing packet") };
        assert!(p.key);
        assert_eq!(p.time, 2.0);
        assert_eq!(loc.start, 3.5);
        // Beyond the live edge: the newest keyframe, starting at the requested time.
        let ahead = st.locate(6.0).unwrap();
        let Next::Packet(p) = s.packet(ahead.index) else { panic!("missing packet") };
        assert_eq!(p.time, 4.0);
        assert_eq!(ahead.start, 6.0);
        // And the packet bytes come back from disk.
        let mut r = ChunkReader::default();
        assert_eq!(r.read(&s, &p).unwrap(), vec![4u8; 10]);
        st.dispose();
    }

    #[test]
    fn puts_reconnects_on_one_continuous_timeline() {
        let st = store();
        let a = st.create_session(streams()).unwrap();
        st.register(&a);
        record(&a, 0.0, 3.0);
        a.finish();
        let b = st.create_session(streams()).unwrap();
        st.register(&b);
        assert_eq!(b.base(), 3.0);
        record(&b, 0.0, 2.0);
        assert_eq!(st.live_edge(), 5.0);
        assert_eq!(st.recorded(), 5.0);
        assert_eq!(st.locate(4.0).unwrap().session.id, b.id);
        assert_eq!(st.locate(1.0).unwrap().session.id, a.id);
        st.dispose();
    }

    #[test]
    fn trims_whole_keyframe_groups() {
        let st = store();
        st.set_buffer_seconds(2.0);
        let s = st.create_session(streams()).unwrap();
        st.register(&s);
        record(&s, 0.0, 12.0);
        st.trim(f64::INFINITY); // cutoff = 12 - 2 - 5 = 5: keep from the keyframe at 4
        assert!(matches!(s.packet(0), Next::Gone));
        let loc = st.locate(0.0).unwrap();
        let Next::Packet(p) = s.packet(loc.index) else { panic!("missing packet") };
        assert_eq!(p.time, 4.0);
        assert_eq!(st.oldest(), 4.0);
        st.dispose();
    }

    #[test]
    fn returns_the_nearest_thumbnail() {
        let st = store();
        for t in [0.0, 1.0, 2.0, 3.0] {
            st.add_thumb(t, Picture::black(2, 2));
        }
        assert_eq!(st.thumb_at(1.4).unwrap().t, 1.0);
        assert_eq!(st.thumb_at(1.6).unwrap().t, 2.0);
        assert_eq!(st.thumb_at(99.0).unwrap().t, 3.0);
        st.dispose();
    }
}
