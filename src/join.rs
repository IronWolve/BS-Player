//! Joining tracks inside one audio pipeline (the base of gapless playback).
//!
//! The decoders of consecutive tracks are chained at the very start of the chain.
//! Everything behind `Joined` (the speed stretcher, the equalizer, the fades and
//! the sink) sees one continuous signal and never learns that a track ended.
//!
//! Two threads meet here. The audio thread runs `Joined` and takes the `Slot`
//! mutex once per track end. The engine thread fills the slot, drains the retire
//! channel (so a finished file is closed there, never inside the audio callback)
//! and reads the `Marks` to learn when the clock reached a boundary.
use crate::playback::{SourceToken, advance_ramp};
use rodio::Source;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU32, Ordering},
        mpsc::Sender,
    },
    time::Duration,
};

/// The live gain is looked at once per this many frames.
const GAIN_POLL_FRAMES: u64 = 1024;
/// A change of the live gain is ramped in over this long, so it never clicks.
const GAIN_RAMP_SECONDS: f32 = 0.05;
/// Pending cut marks are bounded so a stalled engine thread cannot grow them
/// without limit; a join mark is bounded by the preload protocol itself.
const CUT_MARKS_MAX: usize = 4096;

/// A mutex that stays usable after a panic elsewhere: marks and slots hold plain
/// data, and the audio thread must never panic on a poisoned lock.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Linear factor for a gain in dB. A non-finite value is no gain and the range
/// is bounded, so a damaged tag can never put NaN or infinity into the audio.
pub fn db_to_linear(db: f32) -> f32 {
    let db = if db.is_finite() {
        db.clamp(-60., 60.)
    } else {
        0.
    };
    10f32.powf(db / 20.)
}

/// One opened track: samples decoded ahead of time, then the decoder itself.
/// The token is declared last on purpose. Fields drop in order, so by the time
/// it is released the decoder and its file handle are closed, and the open-source
/// counter the engine thread waits on never reports "closed" too early.
pub struct Held {
    prefetch: VecDeque<f32>,
    decoder: Box<dyn Source + Send>,
    channels: u16,
    rate: u32,
    _token: SourceToken,
}
impl Held {
    pub fn new(
        decoder: Box<dyn Source + Send>,
        prefetch: VecDeque<f32>,
        token: SourceToken,
    ) -> Self {
        Self {
            channels: decoder.channels(),
            rate: decoder.sample_rate(),
            prefetch,
            decoder,
            _token: token,
        }
    }
    pub fn channels(&self) -> u16 {
        self.channels
    }
    pub fn sample_rate(&self) -> u32 {
        self.rate
    }
    pub fn total_duration(&self) -> Option<Duration> {
        self.decoder.total_duration()
    }

    /// Decodes up to `samples` (rounded down to whole frames) into the prefetch
    /// buffer and returns how many were read. This runs on the opener thread, so
    /// the first slow read of a file on a network share never reaches the audio
    /// thread.
    pub fn fill_prefetch(&mut self, samples: usize) -> usize {
        let whole = samples - samples % usize::from(self.channels.max(1));
        let mut read = 0;
        while read < whole {
            match self.decoder.next() {
                Some(sample) => self.prefetch.push_back(sample),
                None => break,
            }
            read += 1;
        }
        read
    }
}
impl Iterator for Held {
    type Item = f32;
    #[inline]
    fn next(&mut self) -> Option<f32> {
        if let Some(sample) = self.prefetch.pop_front() {
            return Some(sample);
        }
        self.decoder.next()
    }
}

/// The track that follows the current one, with its ReplayGain in dB and the
/// request id the app will see when the join happens.
pub struct NextSegment {
    pub held: Held,
    pub gain_db: f32,
    pub request_id: u64,
}

/// The hand-over point between the engine thread (producer) and the audio thread
/// (consumer, once per track end).
pub enum Slot {
    /// Nothing is waiting: the current track is the last one.
    Empty,
    Ready(NextSegment),
    /// The audio thread already ran out of samples and found the slot empty: a
    /// later offer would arrive after the end, so it is refused.
    Closed,
}
pub type SharedSlot = Arc<Mutex<Slot>>;
impl Slot {
    pub fn new_shared() -> SharedSlot {
        Arc::new(Mutex::new(Self::Empty))
    }
    /// Offers the next track. Refused (and handed back) when one is waiting
    /// already or the current track has ended for good.
    pub fn put(&mut self, next: NextSegment) -> Result<(), NextSegment> {
        match self {
            Self::Empty => {
                *self = Self::Ready(next);
                Ok(())
            }
            Self::Ready(_) | Self::Closed => Err(next),
        }
    }
    /// Takes back a waiting track the audio thread has not taken yet.
    pub fn take_ready(&mut self) -> Option<NextSegment> {
        match std::mem::replace(self, Self::Empty) {
            Self::Ready(next) => Some(next),
            Self::Closed => {
                *self = Self::Closed;
                None
            }
            Self::Empty => None,
        }
    }
    /// Refuses every later offer and returns what was waiting. The caller drops
    /// it after releasing the lock, so the file closes on the engine thread.
    pub fn close(&mut self) -> Option<NextSegment> {
        match std::mem::replace(self, Self::Closed) {
            Self::Ready(next) => Some(next),
            Self::Empty | Self::Closed => None,
        }
    }
}

/// The new track began at source frame `j`, counted across segments (frames the
/// `Joined` source has produced, silence removal not included).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JoinMark {
    pub j: u64,
    pub request_id: u64,
}
/// A run of `frames` source frames was cut out where the kept-frame index is `k`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // pushed by the silence skipper
pub struct CutMark {
    pub k: u64,
    pub frames: u64,
}
/// What the audio thread pushes upstream and the engine thread applies when the
/// output clock reaches it. Both queues are short and uncontended.
#[derive(Debug, Default)]
pub struct Marks {
    joins: Mutex<VecDeque<JoinMark>>,
    cuts: Mutex<VecDeque<CutMark>>,
}
impl Marks {
    pub fn push_join(&self, mark: JoinMark) {
        lock(&self.joins).push_back(mark);
    }
    #[allow(dead_code)] // pushed by the silence skipper
    pub fn push_cut(&self, mark: CutMark) {
        let mut cuts = lock(&self.cuts);
        cuts.push_back(mark);
        while cuts.len() > CUT_MARKS_MAX {
            cuts.pop_front();
        }
    }
    /// The oldest join whose source-frame index the clock has reached.
    pub fn pop_join_due(&self, j: f64) -> Option<JoinMark> {
        let mut joins = lock(&self.joins);
        match joins.front() {
            Some(mark) if mark.j as f64 <= j => joins.pop_front(),
            _ => None,
        }
    }
    /// Total frames of the cuts whose kept-frame index the clock has reached;
    /// those marks are consumed.
    pub fn take_cuts_due(&self, k: f64) -> u64 {
        let mut cuts = lock(&self.cuts);
        let mut frames = 0u64;
        while let Some(cut) = cuts.front() {
            if cut.k as f64 > k {
                break;
            }
            frames = frames.saturating_add(cut.frames);
            cuts.pop_front();
        }
        frames
    }
    #[allow(dead_code)] // for the engine's consistency checks
    pub fn pending_joins(&self) -> usize {
        lock(&self.joins).len()
    }
}

/// The first stage of the pipeline: the current track's samples, one gain per
/// track, and the hand-over to the next track at its end.
///
/// The gain is the track's ReplayGain. It lives here, not in the shared DSP
/// settings, so that it changes on the exact boundary sample (a gain change
/// anywhere later would take effect after the stretcher's buffered audio and be
/// heard in the wrong track). A linear gain commutes with the stretcher, the
/// equalizer and the mono mix, and the limiter is still last, so the output is the
/// same as applying it at the end.
pub struct Joined {
    current: Held,
    slot: SharedSlot,
    marks: Arc<Marks>,
    /// ReplayGain of the playing track in dB (f32 bits), shared with the engine.
    live_gain: Arc<AtomicU32>,
    gain: f32,
    gain_target: f32,
    gain_ramp_remaining: u32,
    gain_ramp_frames: u32,
    channels: u16,
    rate: u32,
    /// Samples of the current frame already produced.
    in_frame: u16,
    /// Whole frames produced so far, across segments.
    frames: u64,
    /// The current track returned `None`; it is not asked again.
    drained: bool,
    /// Finished engine-side: handed over so the file closes off the audio thread.
    retire: Sender<Held>,
    done: bool,
}
impl Joined {
    pub fn new(
        current: Held,
        slot: SharedSlot,
        marks: Arc<Marks>,
        live_gain: Arc<AtomicU32>,
        retire: Sender<Held>,
    ) -> Self {
        let channels = current.channels().max(1);
        let rate = current.sample_rate();
        let gain = db_to_linear(f32::from_bits(live_gain.load(Ordering::Relaxed)));
        Self {
            current,
            slot,
            marks,
            live_gain,
            gain,
            gain_target: gain,
            gain_ramp_remaining: 0,
            gain_ramp_frames: ((rate as f32 * GAIN_RAMP_SECONDS).round() as u32).max(1),
            channels,
            rate,
            in_frame: 0,
            frames: 0,
            drained: false,
            retire,
            done: false,
        }
    }
    /// Whole frames produced so far, across segments.
    #[allow(dead_code)] // for the engine's consistency checks
    pub fn frames(&self) -> u64 {
        self.frames
    }
    #[inline]
    fn finish_sample(&mut self) {
        self.in_frame += 1;
        if self.in_frame >= self.channels {
            self.in_frame = 0;
            self.frames += 1;
        }
    }
    /// Start of a frame: look at the live gain now and then, and take one step of
    /// a running ramp. All samples of a frame share one gain.
    #[inline]
    fn begin_frame(&mut self) {
        if self.frames % GAIN_POLL_FRAMES == 0 {
            let target = db_to_linear(f32::from_bits(self.live_gain.load(Ordering::Relaxed)));
            if target != self.gain_target {
                self.gain_target = target;
                self.gain_ramp_remaining = self.gain_ramp_frames;
            }
        }
        if self.gain_ramp_remaining > 0 {
            advance_ramp(
                &mut self.gain,
                self.gain_target,
                &mut self.gain_ramp_remaining,
            );
        }
    }
    /// The current track ran dry on a frame boundary: continue with the waiting
    /// one, or close the slot and end. False when the pipeline is over.
    fn join_next(&mut self) -> bool {
        let next = {
            let mut slot = lock(&self.slot);
            match std::mem::replace(&mut *slot, Slot::Closed) {
                Slot::Ready(next) => {
                    *slot = Slot::Empty;
                    Some(next)
                }
                Slot::Empty | Slot::Closed => None,
            }
        };
        let Some(next) = next else {
            self.done = true;
            return false;
        };
        if next.held.channels() != self.channels || next.held.sample_rate() != self.rate {
            // The engine only offers tracks of the same format; if one arrives
            // anyway it is not played (the wrong channel layout would be heard
            // for a whole track). It is retired like any finished track.
            let _ = self.retire.send(next.held);
            self.done = true;
            return false;
        }
        let finished = std::mem::replace(&mut self.current, next.held);
        // The engine thread closes the file. If it is gone, closing here is fine.
        let _ = self.retire.send(finished);
        self.marks.push_join(JoinMark {
            j: self.frames,
            request_id: next.request_id,
        });
        // The new track's gain applies from its first sample, without a ramp.
        let gain_db = if next.gain_db.is_finite() {
            next.gain_db
        } else {
            0.
        };
        self.gain = db_to_linear(gain_db);
        self.gain_target = self.gain;
        self.gain_ramp_remaining = 0;
        self.live_gain.store(gain_db.to_bits(), Ordering::Relaxed);
        self.drained = false;
        true
    }
}
impl Iterator for Joined {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        loop {
            if self.done {
                return None;
            }
            let sample = if self.drained {
                None
            } else {
                self.current.next()
            };
            let Some(sample) = sample else {
                self.drained = true;
                if self.in_frame != 0 {
                    // A decoder that stops mid-frame is completed with silence, so
                    // the next track starts on a frame boundary and its channels
                    // stay where they belong.
                    self.finish_sample();
                    return Some(0.);
                }
                if !self.join_next() {
                    return None;
                }
                continue;
            };
            if self.in_frame == 0 {
                self.begin_frame();
            }
            self.finish_sample();
            return Some(if self.gain == 1. {
                sample
            } else {
                sample * self.gain
            });
        }
    }
}
impl Source for Joined {
    /// Unknown on purpose: the length of one track says nothing about the stream.
    fn current_span_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        self.channels
    }
    fn sample_rate(&self) -> u32 {
        self.rate
    }
    fn total_duration(&self) -> Option<Duration> {
        self.current.total_duration()
    }
}
