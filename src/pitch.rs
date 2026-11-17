//! Bounded, streaming, pitch-preserving playback-speed adapter for Rodio.

use rodio::{Source, source::SeekError};
use std::num::{NonZeroU16, NonZeroU32};
use std::time::Duration;
use wsola::{MAX_TEMPO, MIN_TEMPO, TimeStretch};

const INPUT_CHUNK_SAMPLES: usize = 4_096;
const OUTPUT_CHUNK_SAMPLES: usize = 4_096;

/// A Rodio source that changes tempo without changing pitch.
///
/// Input and output are streamed in bounded, interleaved chunks. A speed of 1.0
/// bypasses WSOLA so normal-speed playback keeps the decoder's original samples.
pub struct PitchSource {
    source: Box<dyn Source + Send>,
    stretcher: Option<TimeStretch>,
    channels: usize,
    speed: f32,
    input: Vec<f32>,
    output: Vec<f32>,
    output_index: usize,
    input_ended: bool,
    flushed: bool,
    input_chunk_samples: usize,
    output_chunk_samples: usize,
}

impl PitchSource {
    pub fn new(source: Box<dyn Source + Send>, speed: f32) -> Self {
        let channels = usize::from(source.channels());
        let sample_rate = source.sample_rate();
        assert!(channels > 0, "decoded audio must have at least one channel");
        assert!(
            sample_rate > 0,
            "decoded audio must have a nonzero sample rate"
        );

        let speed = if speed.is_finite() && speed > 0.0 {
            speed.clamp(MIN_TEMPO, MAX_TEMPO)
        } else {
            1.0
        };
        let stretcher = if speed == 1.0 {
            None
        } else {
            let mut stretcher = TimeStretch::new(
                NonZeroU32::new(sample_rate).expect("validated nonzero sample rate"),
                NonZeroU16::new(channels as u16).expect("validated nonzero channel count"),
            )
            .expect("valid WSOLA sample rate and channel count");
            stretcher.set_tempo(speed);
            Some(stretcher)
        };

        // Keep chunks frame-aligned, with a strict sample-count bound even for
        // unusual multichannel sources.
        let input_chunk_samples = (INPUT_CHUNK_SAMPLES / channels).max(1) * channels;
        let output_chunk_samples = (OUTPUT_CHUNK_SAMPLES / channels).max(1) * channels;

        Self {
            source,
            stretcher,
            channels,
            speed,
            input: Vec::with_capacity(input_chunk_samples),
            output: Vec::with_capacity(output_chunk_samples),
            output_index: 0,
            input_ended: false,
            flushed: false,
            input_chunk_samples,
            output_chunk_samples,
        }
    }

    fn next_stretched_sample(&mut self) -> Option<f32> {
        loop {
            if self.output_index < self.output.len() {
                let sample = self.output[self.output_index];
                self.output_index += 1;
                return Some(sample);
            }

            self.output.clear();
            self.output_index = 0;

            let stretcher = self.stretcher.as_mut()?;
            if stretcher.available() > 0 {
                self.output
                    .extend(stretcher.pull(self.output_chunk_samples));
                continue;
            }

            if self.input_ended {
                if self.flushed {
                    return None;
                }
                self.output.extend(stretcher.flush());
                self.flushed = true;
                continue;
            }

            self.input.clear();
            while self.input.len() < self.input_chunk_samples {
                match self.source.next() {
                    Some(sample) => self.input.push(sample),
                    None => {
                        self.input_ended = true;
                        break;
                    }
                }
            }

            // A broken decoder may end partway through a channel frame. Drop
            // only that incomplete final frame; WSOLA requires aligned input.
            let complete_samples = self.input.len() / self.channels * self.channels;
            self.input.truncate(complete_samples);
            if !self.input.is_empty() {
                stretcher.push(&self.input);
            }
            self.output
                .extend(stretcher.pull(self.output_chunk_samples));
        }
    }
}

impl Iterator for PitchSource {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.stretcher.is_none() {
            return self.source.next();
        }
        self.next_stretched_sample()
    }
}

impl Source for PitchSource {
    fn current_span_len(&self) -> Option<usize> {
        if self.stretcher.is_none() {
            self.source.current_span_len()
        } else {
            None
        }
    }

    fn channels(&self) -> u16 {
        self.source.channels()
    }

    fn sample_rate(&self) -> u32 {
        self.source.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        let duration = self.source.total_duration()?;
        if self.stretcher.is_none() {
            Some(duration)
        } else {
            Some(Duration::from_secs_f64(
                duration.as_secs_f64() / f64::from(self.speed),
            ))
        }
    }

    fn try_seek(&mut self, _pos: Duration) -> Result<(), SeekError> {
        Err(SeekError::NotSupported {
            underlying_source: std::any::type_name::<Self>(),
        })
    }
}
