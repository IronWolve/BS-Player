//! Skipping long silences (audiobooks and podcasts).
//!
//! `SkipSilence` sits directly behind `Joined` and in front of the speed
//! stretcher, so it sees the decoded signal after ReplayGain and works in source
//! time. A silent stretch longer than 0.6 s is shortened to a quarter of a
//! second: the first 0.15 s and the last 0.1 s of the stretch stay (so a pause
//! still sounds like a pause and a breath before a word is not clipped), a 5 ms
//! crossfade joins the two, and the rest is dropped. Every drop is reported to
//! the engine thread as a `CutMark`, so the player's position, bookmarks, the
//! seek bar and the chapter marks keep counting source time: the position
//! advances by the removed time at the moment the splice is heard.
//!
//! The stage works on blocks of 10 ms (levels over all channels at once) with a
//! small state machine:
//!
//! * Sound (`Idle`): blocks pass untouched. A block whose RMS is under -50 dBFS
//!   and whose peak is under -40 dBFS starts a quiet run.
//! * `Head`: the run's first 0.15 s are emitted as they come.
//! * `Look`: further quiet blocks are read ahead into a buffer (at most 0.45 s)
//!   without being emitted. If sound returns before the run is over 0.6 s long,
//!   the buffer is emitted untouched and nothing was cut.
//! * `Dropping`: the run is over 0.6 s. Only the 5 ms that followed the head and
//!   a ring with the latest 0.1 s are kept; everything between is counted and
//!   dropped. On the first loud block, at 10 s of removed audio (one splice never
//!   removes more) or at the end of the input, the crossfade and the ring are
//!   emitted and the cut is announced.
//!
//! Staying in a run is judged more leniently than entering it (RMS under -46 dBFS
//! and a peak under -40 dBFS), so a noise floor hovering near the threshold does
//! not chatter in and out of cuts.
//!
//! Disabled (the shared flag is off, read once per block), the stage hands the
//! decoder's samples straight through, bit for bit and without added latency, as
//! soon as its buffers are empty. Frames are conserved: what comes out plus what
//! was cut is exactly what went in. A NaN or infinite sample counts as loud.
use crate::join::{CutMark, Marks};
use rodio::Source;
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// Length of one analysis block.
const BLOCK_MS: u64 = 10;
/// A run starts at a block whose mean square is under (10^(-50/20))^2 = 1e-5
/// (-50 dBFS RMS) and whose peak is under -40 dBFS.
const ENTER_MEAN_SQUARE: f32 = 1e-5;
/// A run goes on while the mean square is under 10^(-46/10) (-46 dBFS RMS) and
/// the peak is under -40 dBFS.
const STAY_MEAN_SQUARE: f32 = 2.511_886_4e-5;
/// -40 dBFS as a sample magnitude.
const PEAK_LIMIT: f32 = 0.01;
/// The start of a run that is always played (0.15 s of 10 ms blocks).
const KEEP_HEAD_BLOCKS: u32 = 15;
/// Blocks read ahead before a run counts as long (0.45 s).
const LOOKAHEAD_BLOCKS: usize = 45;
/// The shortest run that is cut: the head plus the look-ahead, 0.6 s. Exactly
/// this long is left alone; one block more is cut.
#[allow(dead_code)] // documents the rule; the test checks it against the blocks
const MIN_RUN_BLOCKS: u32 = KEEP_HEAD_BLOCKS + LOOKAHEAD_BLOCKS as u32;
/// The end of a cut run that is always played (0.1 s).
const KEEP_TAIL_MS: u64 = 100;
/// Length of the crossfade that joins the head to the tail.
const CROSSFADE_MS: u64 = 5;
/// One splice never removes more than this (seconds of source time).
const MAX_CUT_SECONDS: u64 = 10;
/// Formats outside this range are passed through untouched, never analysed.
const MIN_RATE: u32 = 1_000;
const MAX_RATE: u32 = 768_000;
const MAX_CHANNELS: usize = 32;
/// The buffers are sized once, up front; this bounds that for absurd formats.
const RESERVE_MAX_SAMPLES: usize = 2_097_152;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Sound, or a quiet stretch that has not started yet.
    Idle,
    /// The first part of a quiet run, played as it comes.
    Head,
    /// Quiet blocks are being held back until the run proves long enough.
    Look,
    /// The run is long: the middle is dropped, a ring of the latest audio is kept.
    Dropping,
}

pub struct SkipSilence {
    source: Box<dyn Source + Send>,
    enabled: Arc<AtomicBool>,
    marks: Arc<Marks>,
    source_channels: u16,
    rate: u32,
    channels: usize,
    block_samples: usize,
    crossfade_frames: usize,
    tail_frames: usize,
    max_cut_frames: u64,
    phase: Phase,
    /// Blocks of the current run played so far while in `Head`.
    head_blocks: u32,
    /// Blocks held back while in `Look`.
    look_blocks: usize,
    /// Frames dropped in the splice being built (the crossfaded part not included).
    dropped: u64,
    /// The first frames after the head: the part that fades out in the crossfade.
    after_head: Vec<f32>,
    /// The held-back run (`Look`), then its latest part (`Dropping`).
    hold: VecDeque<f32>,
    /// Samples ready to be handed out.
    out: VecDeque<f32>,
    /// The block being analysed.
    block: Vec<f32>,
    /// Samples of a last, incomplete frame (a decoder that stops mid-frame).
    trailer: Vec<f32>,
    /// Samples handed to the output so far, queued ones included: the splice
    /// position `k` is counted in the frames the stage has produced.
    produced: u64,
    /// Samples still to pass straight through before the switch is read again.
    bypass_left: usize,
    finished: bool,
}

impl SkipSilence {
    pub fn new(
        source: Box<dyn Source + Send>,
        enabled: Arc<AtomicBool>,
        marks: Arc<Marks>,
    ) -> Self {
        let source_channels = source.channels();
        let rate = source.sample_rate();
        let usable = (1..=MAX_CHANNELS as u16).contains(&source_channels)
            && (MIN_RATE..=MAX_RATE).contains(&rate);
        // An unsupported format is never analysed: it is passed through for good.
        let channels = if usable { usize::from(source_channels) } else { 1 };
        let rate_checked = if usable { rate } else { MIN_RATE };
        let block_frames = (u64::from(rate_checked) * BLOCK_MS / 1000).max(1) as usize;
        let block_samples = block_frames * channels;
        let tail_frames = (u64::from(rate_checked) * KEEP_TAIL_MS / 1000).max(1) as usize;
        let crossfade_frames = (u64::from(rate_checked) * CROSSFADE_MS / 1000).max(1) as usize;
        let reserve = |samples: usize| samples.min(RESERVE_MAX_SAMPLES);
        let (hold, out, block, after_head, trailer) = if usable {
            (
                // Both queues can hold a whole held run (and the block that ends
                // it): flushing a run swaps them, so neither ever has to grow.
                VecDeque::with_capacity(reserve((LOOKAHEAD_BLOCKS + 2) * block_samples)),
                VecDeque::with_capacity(reserve((LOOKAHEAD_BLOCKS + 2) * block_samples)),
                Vec::with_capacity(block_samples),
                Vec::with_capacity(reserve(crossfade_frames * channels)),
                Vec::with_capacity(channels),
            )
        } else {
            (
                VecDeque::new(),
                VecDeque::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
        };
        Self {
            source,
            enabled,
            marks,
            source_channels,
            rate,
            channels,
            block_samples,
            crossfade_frames,
            tail_frames,
            max_cut_frames: u64::from(rate_checked) * MAX_CUT_SECONDS,
            phase: Phase::Idle,
            head_blocks: 0,
            look_blocks: 0,
            dropped: 0,
            after_head,
            hold,
            out,
            block,
            trailer,
            produced: 0,
            bypass_left: if usable { 0 } else { usize::MAX },
            finished: false,
        }
    }

    /// Reads the next block (10 ms of frames) into `block`; true when the source
    /// ended during it.
    fn read_block(&mut self) -> bool {
        self.block.clear();
        while self.block.len() < self.block_samples {
            match self.source.next() {
                Some(sample) => self.block.push(sample),
                None => return true,
            }
        }
        false
    }

    /// Makes `out` non-empty (or the stage finished). Runs only when `out` is
    /// empty, once per block of input.
    fn fill(&mut self) {
        let on = self.enabled.load(Ordering::Relaxed);
        if !on && self.phase == Phase::Idle {
            // Nothing is held and nothing is wanted: pass samples straight
            // through for one block, then look at the switch again.
            self.bypass_left = self.block_samples;
            return;
        }
        let ended = self.read_block();
        let stray = self.block.len() % self.channels;
        if stray != 0 {
            // The source ended inside a frame: the loose samples go last.
            let keep = self.block.len() - stray;
            self.trailer.extend_from_slice(&self.block[keep..]);
            self.block.truncate(keep);
        }
        if !self.block.is_empty() {
            self.step(on);
        }
        if ended {
            self.finish();
        }
    }

    /// One block through the state machine. `on` false counts as "sound": a run
    /// that is open ends here, with every frame accounted for.
    fn step(&mut self, on: bool) {
        let (mean_square, loud_peak) = measure(&self.block);
        // `<` is false for NaN, so a damaged sample is never quiet.
        let starts = on && mean_square < ENTER_MEAN_SQUARE && !loud_peak;
        let stays = on && mean_square < STAY_MEAN_SQUARE && !loud_peak;
        match self.phase {
            Phase::Idle => {
                if starts {
                    self.phase = Phase::Head;
                    self.head_blocks = 1;
                }
                self.emit_block();
            }
            Phase::Head => {
                if stays {
                    self.head_blocks += 1;
                    if self.head_blocks >= KEEP_HEAD_BLOCKS {
                        self.phase = Phase::Look;
                        self.look_blocks = 0;
                    }
                } else {
                    self.phase = Phase::Idle;
                }
                self.emit_block();
            }
            Phase::Look => {
                if !stays {
                    self.flush_hold();
                    self.phase = Phase::Idle;
                    self.emit_block();
                } else if self.look_blocks < LOOKAHEAD_BLOCKS {
                    self.hold.extend(self.block.iter());
                    self.look_blocks += 1;
                } else {
                    self.begin_drop();
                    self.drop_block();
                }
            }
            Phase::Dropping => {
                if stays {
                    self.drop_block();
                } else {
                    self.splice();
                    self.phase = Phase::Idle;
                    self.emit_block();
                }
            }
        }
    }

    /// The block goes out as it is.
    fn emit_block(&mut self) {
        self.out.extend(self.block.iter());
        self.produced += self.block.len() as u64;
    }

    /// The held-back run was not long enough: it goes out untouched.
    fn flush_hold(&mut self) {
        self.produced += self.hold.len() as u64;
        if self.out.is_empty() {
            // The usual case: the held run simply becomes the output.
            std::mem::swap(&mut self.out, &mut self.hold);
        } else {
            self.out.extend(self.hold.drain(..));
        }
        self.look_blocks = 0;
    }

    /// The run turned out to be over 0.6 s: keep the 5 ms after the head aside
    /// (it fades out in the crossfade) and go on keeping only the latest audio.
    fn begin_drop(&mut self) {
        let keep = (self.crossfade_frames * self.channels).min(self.hold.len());
        self.after_head.clear();
        self.after_head.extend(self.hold.iter().take(keep));
        self.hold.drain(..keep);
        self.dropped = 0;
        self.phase = Phase::Dropping;
    }

    /// A quiet block of a long run: it joins the ring, and what falls out of the
    /// ring is dropped (and counted).
    fn drop_block(&mut self) {
        self.hold.extend(self.block.iter());
        let keep = self.tail_frames * self.channels;
        if self.hold.len() > keep {
            let excess = self.hold.len() - keep;
            self.hold.drain(..excess);
            self.dropped += (excess / self.channels) as u64;
        }
        let removed = (self.after_head.len() / self.channels) as u64 + self.dropped;
        if removed >= self.max_cut_frames {
            // One splice never removes more than 10 s; the run goes on afterwards
            // (what was just played counts as its head).
            self.splice();
            self.phase = Phase::Look;
            self.look_blocks = 0;
        }
    }

    /// Ends a cut: the crossfade from the part after the head into the first
    /// frames of the ring, then the rest of the ring. The cut is announced with
    /// the frame index where the ring begins.
    fn splice(&mut self) {
        let channels = self.channels;
        let head_frames = self.after_head.len() / channels;
        let cut = head_frames as u64 + self.dropped;
        if cut > 0 {
            self.marks.push_cut(CutMark {
                k: self.produced / channels as u64,
                frames: cut,
            });
        }
        let tail = self.hold.make_contiguous();
        let fade = head_frames.min(tail.len() / channels);
        for frame in 0..fade {
            let t = (frame + 1) as f32 / (fade + 1) as f32;
            for channel in 0..channels {
                let at = frame * channels + channel;
                let (from, to) = (self.after_head[at], tail[at]);
                self.out.push_back(from + (to - from) * t);
            }
        }
        self.out.extend(tail[fade * channels..].iter());
        self.produced += tail.len() as u64;
        self.hold.clear();
        self.after_head.clear();
        self.dropped = 0;
    }

    /// The source ended: whatever is held is resolved, loose samples go last.
    fn finish(&mut self) {
        match self.phase {
            Phase::Look => self.flush_hold(),
            Phase::Dropping => self.splice(),
            Phase::Idle | Phase::Head => {}
        }
        self.phase = Phase::Idle;
        self.out.extend(self.trailer.drain(..));
        self.finished = true;
    }
}

/// Mean square over all samples of the block and whether any sample reaches
/// -40 dBFS. NaN and infinity make both read as loud.
fn measure(block: &[f32]) -> (f32, bool) {
    let mut sum = 0f32;
    let mut loud = false;
    for &sample in block {
        sum += sample * sample;
        loud |= !(sample.abs() < PEAK_LIMIT);
    }
    (sum / block.len().max(1) as f32, loud)
}

impl Iterator for SkipSilence {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        loop {
            if let Some(sample) = self.out.pop_front() {
                return Some(sample);
            }
            if self.bypass_left > 0 {
                self.bypass_left -= 1;
                return match self.source.next() {
                    Some(sample) => {
                        self.produced += 1;
                        Some(sample)
                    }
                    None => {
                        self.bypass_left = 0;
                        self.finished = true;
                        None
                    }
                };
            }
            if self.finished {
                return None;
            }
            self.fill();
        }
    }
}

impl Source for SkipSilence {
    /// Unknown on purpose, like the stage in front of it: spans mean nothing for
    /// a stream of joined, shortened tracks.
    fn current_span_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        self.source_channels
    }
    fn sample_rate(&self) -> u32 {
        self.rate
    }
    fn total_duration(&self) -> Option<Duration> {
        self.source.total_duration()
    }
}
