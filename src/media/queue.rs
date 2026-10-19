//! Decoded frames on their way from a buffer reader to the playout.

use std::collections::VecDeque;

use crate::media::picture::Picture;

pub const AUDIO_RATE: u32 = 48_000;
pub const AUDIO_CHANNELS: usize = 2;

/// One output frame: a picture and the interleaved s16 stereo audio that plays with it.
#[derive(Clone)]
pub struct AvFrame {
    pub video: Picture,
    pub audio: Vec<i16>,
}

/// Interleaved audio samples (both channels) belonging to one frame at `fps`.
pub fn samples_per_frame(fps: u32) -> usize {
    (AUDIO_RATE / fps) as usize * AUDIO_CHANNELS
}

/// Pairs video frame N with audio samples [N*spf, (N+1)*spf) of the same stream, so
/// dropping, repeating or skipping frames never pulls audio and video apart.
pub struct AvQueue {
    fps: u32,
    per_frame: usize,
    frames: VecDeque<Picture>,
    /// absolute index of frames[0]
    pub next: u64,
    audio: VecDeque<i16>,
    /// absolute sample position of the first queued audio sample
    audio_head: u64,
}

impl AvQueue {
    pub fn new(fps: u32) -> Self {
        AvQueue {
            fps,
            per_frame: samples_per_frame(fps),
            frames: VecDeque::new(),
            next: 0,
            audio: VecDeque::new(),
            audio_head: 0,
        }
    }

    pub fn push_video(&mut self, frame: Picture) {
        self.frames.push_back(frame);
    }

    pub fn push_audio(&mut self, samples: &[i16]) {
        self.audio.extend(samples);
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    fn skip_audio(&mut self, n: usize) {
        let n = n.min(self.audio.len());
        self.audio.drain(..n);
        self.audio_head += n as u64;
    }

    /// Drop audio belonging to frames that were already consumed or dropped.
    fn align_audio(&mut self) {
        let want = self.next * self.per_frame as u64;
        if self.audio_head < want {
            self.skip_audio((want - self.audio_head) as usize);
        }
        // Never let audio run more than 3 s ahead of video (e.g. video stalled).
        let max_ahead = self.per_frame * self.fps as usize * 3;
        if self.audio.len() > max_ahead {
            self.skip_audio(self.audio.len() - max_ahead);
        }
    }

    pub fn has_audio_for_next(&mut self) -> bool {
        self.align_audio();
        self.audio_head == self.next * self.per_frame as u64 && self.audio.len() >= self.per_frame
    }

    /// The next frame with its audio, or None when it isn't available yet.
    /// With `force`, missing audio is replaced by silence instead of waiting for it.
    pub fn pull(&mut self, force: bool) -> Option<AvFrame> {
        if self.frames.is_empty() || (!self.has_audio_for_next() && !force) {
            return None;
        }
        let video = self.frames.pop_front()?;
        let mut audio = vec![0i16; self.per_frame];
        if self.audio_head == self.next * self.per_frame as u64 {
            let n = self.per_frame.min(self.audio.len());
            for (dst, src) in audio.iter_mut().zip(self.audio.drain(..n)) {
                *dst = src;
            }
            self.audio_head += n as u64;
        }
        self.next += 1;
        Some(AvFrame { video, audio })
    }

    pub fn clear(&mut self) {
        self.next += self.frames.len() as u64;
        self.frames.clear();
        self.align_audio();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FPS: u32 = 30;

    fn pic(n: u8) -> Picture {
        Picture::filled(2, 2, n, 128)
    }

    /// Audio whose sample values encode the frame they belong to.
    fn audio_for(frame: usize, frames: usize) -> Vec<i16> {
        let spf = samples_per_frame(FPS);
        (0..frames * spf).map(|i| (frame + i / spf) as i16).collect()
    }

    #[test]
    fn pairs_each_frame_with_its_own_audio() {
        let mut q = AvQueue::new(FPS);
        for i in 0..3 {
            q.push_video(pic(i));
        }
        q.push_audio(&audio_for(0, 3));
        for i in 0..3 {
            let out = q.pull(false).unwrap();
            assert_eq!(out.video.y()[0], i);
            assert_eq!(out.audio[0], i as i16);
        }
    }

    #[test]
    fn waits_for_late_audio_unless_forced() {
        let mut q = AvQueue::new(FPS);
        q.push_video(pic(0));
        assert!(q.pull(false).is_none());
        let forced = q.pull(true).unwrap();
        assert!(forced.audio.iter().all(|&s| s == 0));
    }

    #[test]
    fn keeps_sync_after_skipping_frames() {
        let mut q = AvQueue::new(FPS);
        for i in 0..10 {
            q.push_video(pic(i));
        }
        q.push_audio(&audio_for(0, 10));
        for _ in 0..7 {
            q.pull(true); // skipped, as a reader skips to catch up
        }
        let out = q.pull(false).unwrap();
        assert_eq!(out.video.y()[0], 7);
        assert_eq!(out.audio[0], 7);
    }

    #[test]
    fn discards_audio_for_frames_skipped_before_the_audio_arrived() {
        let mut q = AvQueue::new(FPS);
        for i in 0..5 {
            q.push_video(pic(i));
        }
        for _ in 0..4 {
            q.pull(true); // skip frames 0..3 while no audio is queued yet
        }
        q.push_audio(&audio_for(0, 5));
        let out = q.pull(false).unwrap();
        assert_eq!(out.video.y()[0], 4);
        assert_eq!(out.audio[0], 4);
    }
}
