//! Small shared helpers for selecting audio from multiplexed containers.
//!
//! MP4 and Matroska may contain video and subtitle tracks before their audio
//! tracks. Keep the selection policy here so metadata and playback agree.

use std::{
    io::{self, Read, Seek, SeekFrom},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use symphonia::{
    core::{
        audio::SampleBuffer,
        codecs::{
            CodecParameters, CODEC_TYPE_OPUS, CODEC_TYPE_VORBIS, Decoder, DecoderOptions,
        },
        errors::Error as DemuxError,
        formats::{FormatOptions, FormatReader, Packet, SeekMode, SeekTo, Track},
        io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions},
        units::TimeBase,
    },
    default::{formats::{IsoMp4Reader, MkvReader}, get_codecs},
};
use rodio::{Source, source::SeekError};

/// The container is identified from its signature before Symphonia is asked to
/// probe it. WebM uses the same EBML signature and is handled by the Matroska
/// demuxer as well.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ContainerKind {
    Mp4,
    Matroska,
}

/// A selected audio track and the demuxer that owns its packets.
pub(crate) struct OpenedAudio {
    pub(crate) reader: Box<dyn FormatReader>,
    pub(crate) track_id: u32,
    pub(crate) codec_params: CodecParameters,
    /// Effective duration for this selected track, after container trim data.
    pub(crate) duration: Option<Duration>,
    /// Whether duration can safely cap decoded samples. Some containers provide
    /// an audio-only estimate without an exact terminal trim boundary.
    pub(crate) duration_is_exact: bool,
    pub(crate) timing: AudioTiming,
}

/// Presentation timeline adjustments reported by the container demuxer.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct AudioTiming {
    /// Leading audible boundary in raw decoded-frame coordinates, expressed at
    /// the selected audio sample rate. Packet timestamps remain unshifted.
    pub(crate) timestamp_offset_frames: u64,
    /// Decoder warm-up before a seek target, in selected-audio sample frames.
    pub(crate) seek_preroll_frames: u64,
    /// Matroska timestamps may be rounded to milliseconds. In that case packet
    /// order and decoded frame counts define progression, not inferred gaps.
    pub(crate) timestamps_are_quantized: bool,
}

/// Properties useful to metadata and playback callers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AudioProperties {
    pub(crate) sample_rate: u32,
    pub(crate) channels: u16,
    pub(crate) duration: Option<Duration>,
    pub(crate) duration_is_exact: bool,
}

/// Identify an MP4 or EBML container from its leading bytes and restore the
/// original position even when the read is short or fails.
pub(crate) fn detect_container<R: Read + Seek>(input: &mut R) -> Result<Option<ContainerKind>, String> {
    let position = input.stream_position().map_err(|error| error.to_string())?;
    input
        .seek(SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    let mut header = [0u8; 12];
    let mut filled = 0;
    let mut read_error = None;
    while filled < header.len() {
        match input.read(&mut header[filled..]) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => {
                read_error = Some(error.to_string());
                break;
            }
        }
    }
    let restored = input.seek(SeekFrom::Start(position)).map_err(|error| error.to_string());
    if let Some(error) = read_error {
        let _ = restored;
        return Err(error);
    }
    restored?;

    let kind = if filled >= 8 && &header[4..8] == b"ftyp" {
        Some(ContainerKind::Mp4)
    } else if filled >= 4 && header[..4] == [0x1a, 0x45, 0xdf, 0xa3] {
        Some(ContainerKind::Matroska)
    } else {
        None
    };
    Ok(kind)
}

/// Return the default track when it has an enabled audio codec; otherwise choose
/// the first enabled audio codec in container order. Explicitly permit Opus,
/// whose packet decoder is provided by this application rather than Symphonia.
pub(crate) fn select_supported_audio_track<'a>(
    default_track: Option<&'a Track>,
    tracks: &'a [Track],
) -> Option<&'a Track> {
    let supported = |track: &&Track| supported_audio_track(&track.codec_params);
    default_track
        .filter(|track| supported(track))
        .or_else(|| tracks.iter().find(supported))
}

fn supported_audio_track(params: &CodecParameters) -> bool {
    if params.codec == CODEC_TYPE_OPUS {
        return params
            .extra_data
            .as_deref()
            .is_some_and(is_supported_opus_head);
    }
    get_codecs()
        .make(params, &DecoderOptions::default())
        .is_ok()
}

fn is_supported_opus_head(extra: &[u8]) -> bool {
    extra.len() >= 19
        && extra.starts_with(b"OpusHead")
        && extra[8] <= 0x0f
        && matches!(extra[9], 1 | 2)
        && extra[18] == 0
}

/// Read the selected track's basic properties. Opus is always decoded at 48 kHz;
/// its channel count can be absent in MP4 codec parameters and is recovered from
/// the container's Opus identification header.
pub(crate) fn audio_properties(opened: &OpenedAudio) -> Option<AudioProperties> {
    let params = &opened.codec_params;
    let (sample_rate, channels) = if params.codec == CODEC_TYPE_OPUS {
        let extra = params.extra_data.as_deref()?;
        let head = opus_head(extra)?;
        (48_000, u16::from(head[9]))
    } else {
        // Several container sample entries omit channel layout from their codec
        // parameters. A freshly constructed decoder has the parsed signal spec
        // available in `last_decoded()` without consuming the first packet.
        let spec = get_codecs()
            .make(params, &DecoderOptions::default())
            .ok()
            .map(|decoder| *decoder.last_decoded().spec());
        let sample_rate = spec
            .map(|spec| spec.rate)
            .filter(|&rate| rate > 0)
            .or(params.sample_rate)?;
        let channels = spec
            .map(|spec| spec.channels.count())
            .filter(|&channels| channels > 0)
            .or_else(|| params.channels.map(|channels| channels.count()))
            .or_else(|| params.channel_layout.map(|layout| layout.into_channels().count()))?;
        (sample_rate, u16::try_from(channels).ok()?)
    };
    Some(AudioProperties {
        sample_rate,
        channels,
        duration: opened.duration,
        duration_is_exact: opened.duration_is_exact,
    })
}

/// Build a decoder over the exact selected container track. Packet trims are
/// consumed here once; no Rodio container probe or second track-selection rule
/// is involved.
pub(crate) fn open_symphonia_audio(
    opened: OpenedAudio,
) -> Result<Box<dyn Source + Send>, String> {
    let decoder = get_codecs()
        .make(&opened.codec_params, &DecoderOptions::default())
        .map_err(|error| error.to_string())?;
    let spec = *decoder.last_decoded().spec();
    // Vorbis overlap-add depends on the preceding packet. Matroska commonly
    // omits SeekPreRoll for Vorbis, so restart one long decoder window early to
    // rebuild lapping state before exposing samples at the seek target.
    let codec_preroll_frames = if opened.codec_params.codec == CODEC_TYPE_VORBIS {
        decoder.last_decoded().capacity() as u64
    } else {
        0
    };
    let rate = (spec.rate > 0)
        .then_some(spec.rate)
        .or(decoder.codec_params().sample_rate)
        .or(opened.codec_params.sample_rate)
        .ok_or_else(|| "The selected audio track has no sample rate".to_string())?;
    let channels = (spec.channels.count() > 0)
        .then_some(spec.channels.count())
        .or_else(|| decoder.codec_params().channels.map(|channels| channels.count()))
        .or_else(|| opened.codec_params.channels.map(|channels| channels.count()))
        .or_else(|| {
            decoder.codec_params().channel_layout.or(opened.codec_params.channel_layout)
                .map(|layout| layout.into_channels().count())
        })
        .ok_or_else(|| "The selected audio track has no channel layout".to_string())?;
    if rate == 0 || !(1..=32).contains(&channels) {
        return Err(format!("Unsupported audio format: {channels} channels at {rate} Hz"));
    }
    let time_base = opened
        .codec_params
        .time_base
        .ok_or_else(|| "The selected audio track has no timestamp time base".to_string())?;
    let mut source = ContainerAudioSource {
        reader: opened.reader,
        track_id: opened.track_id,
        decoder,
        channels,
        sample_rate: rate,
        time_base,
        timing: opened.timing,
        codec_preroll_frames,
        duration: opened.duration,
        duration_is_exact: opened.duration_is_exact,
        samples: Vec::new(),
        sample_pos: 0,
        keep_from: opened.timing.timestamp_offset_frames,
        next_frame: None,
        decode_errors: 0,
        finished: false,
    };
    if source.codec_preroll_frames > 0 && source.timing.timestamp_offset_frames == 0 {
        // Vorbis' first packet initializes lapping state but may emit no samples.
        // Prime through that packet and cache the first nonempty audio block so
        // the source origin is the first timestamp that maps to decoded frames.
        if !source.refill() {
            return Err("The selected Vorbis track contains no decodable audio".to_string());
        }
    }
    Ok(Box::new(source))
}

const MAX_DECODED_PACKET_FRAMES: usize = 480_000;
const MAX_PACKETS_PER_REFILL: usize = 4096;
const MAX_DECODE_ERRORS: u32 = 64;

struct ContainerAudioSource {
    reader: Box<dyn FormatReader>,
    track_id: u32,
    decoder: Box<dyn Decoder>,
    channels: usize,
    sample_rate: u32,
    time_base: TimeBase,
    timing: AudioTiming,
    codec_preroll_frames: u64,
    duration: Option<Duration>,
    duration_is_exact: bool,
    samples: Vec<f32>,
    sample_pos: usize,
    keep_from: u64,
    next_frame: Option<u64>,
    decode_errors: u32,
    finished: bool,
}

impl ContainerAudioSource {
    fn packet_frame(&self, packet: &Packet) -> u64 {
        ticks_to_frames(packet.ts, self.time_base, self.sample_rate)
    }

    fn refill(&mut self) -> bool {
        self.samples.clear();
        self.sample_pos = 0;
        if self.finished {
            return false;
        }
        let mut work = 0;
        while work < MAX_PACKETS_PER_REFILL {
            work += 1;
            let mut packet = match self.reader.next_packet() {
                Ok(packet) => packet,
                Err(DemuxError::DecodeError(_)) => {
                    self.decode_errors = self.decode_errors.saturating_add(1);
                    if self.decode_errors > MAX_DECODE_ERRORS {
                        self.finished = true;
                        return false;
                    }
                    continue;
                }
                Err(DemuxError::IoError(error))
                    if error.kind() == io::ErrorKind::UnexpectedEof =>
                {
                    self.finished = true;
                    return false;
                }
                Err(DemuxError::ResetRequired) => {
                    self.finished = true;
                    return false;
                }
                Err(_) => {
                    self.finished = true;
                    return false;
                }
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            if packet.data.is_empty() {
                continue;
            }
            let frame = self.packet_frame(&packet);
            let trim_start = packet.trim_start as usize;
            let trim_end = packet.trim_end as usize;
            // Symphonia's Vorbis decoder consumes Packet trim fields internally.
            // Clear them before decode and apply the captured sample cuts once in
            // this adapter, the same way the custom Opus source does.
            packet.trim_start = 0;
            packet.trim_end = 0;
            let decoded = match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    self.decode_errors = 0;
                    decoded
                }
                Err(DemuxError::DecodeError(_)) => {
                    self.decode_errors = self.decode_errors.saturating_add(1);
                    if self.decode_errors > MAX_DECODE_ERRORS {
                        self.finished = true;
                        return false;
                    }
                    continue;
                }
                Err(_) => {
                    self.finished = true;
                    return false;
                }
            };
            let frames = decoded.frames();
            let spec = *decoded.spec();
            if frames == 0 {
                continue;
            }
            if self.next_frame.is_none() {
                if self.codec_preroll_frames > 0 && self.timing.timestamp_offset_frames == 0 {
                    self.timing.timestamp_offset_frames = frame;
                    self.keep_from = frame;
                }
                self.next_frame = Some(frame);
            }
            if frames > MAX_DECODED_PACKET_FRAMES
                || spec.rate != self.sample_rate
                || spec.channels.count() != self.channels
            {
                self.finished = true;
                return false;
            }
            let mut converted = SampleBuffer::<f32>::new(frames as u64, spec);
            converted.copy_interleaved_ref(decoded);
            let start = self.next_frame.unwrap_or(frame);
            let end = start.saturating_add(frames as u64);
            self.next_frame = Some(end);
            // The audible interval is [offset, offset + effective duration).
            // Intersect that interval with the packet's decoded-frame trim
            // interval; do not add container delay to Packet.trim_start.
            let from = (self.keep_from.saturating_sub(start).min(frames as u64) as usize)
                .max(trim_start)
                .min(frames);
            let mut to = frames.saturating_sub(trim_end);
            if self.duration_is_exact {
                if let Some(duration) = self.duration {
                    let total = self
                        .timing
                        .timestamp_offset_frames
                        .saturating_add(duration_to_frames(duration, self.sample_rate));
                    to = to.min(total.saturating_sub(start).min(frames as u64) as usize);
                    if end >= total {
                        self.finished = true;
                    }
                }
            }
            if from >= to {
                if self.finished {
                    return false;
                }
                continue;
            }
            let first = from.saturating_mul(self.channels);
            let last = to.saturating_mul(self.channels);
            self.samples.extend(
                converted
                    .samples()
                    .get(first..last)
                    .unwrap_or_default()
                    .iter()
                    .map(|sample| if sample.is_finite() { *sample } else { 0. }),
            );
            return !self.samples.is_empty();
        }
        // A pathological mux or a run of fully-trimmed packets cannot monopolize
        // the caller's refill loop.
        self.finished = true;
        false
    }
}

impl Iterator for ContainerAudioSource {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.sample_pos >= self.samples.len() && !self.refill() {
            return None;
        }
        let sample = self.samples.get(self.sample_pos).copied();
        self.sample_pos += usize::from(sample.is_some());
        sample
    }
}

impl Source for ContainerAudioSource {
    fn current_span_len(&self) -> Option<usize> {
        // Packet-buffer boundaries are not format boundaries or end of stream.
        // Returning Some(0) before refill makes Rodio's converters stop early.
        None
    }

    fn channels(&self) -> u16 {
        self.channels as u16
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        self.duration
    }

    fn try_seek(&mut self, position: Duration) -> Result<(), SeekError> {
        let mut target = duration_to_frames(position, self.sample_rate);
        if self.duration_is_exact {
            if let Some(duration) = self.duration {
                target = target.min(duration_to_frames(duration, self.sample_rate));
            }
        }
        let native_target = target
            .saturating_add(self.timing.timestamp_offset_frames)
            .saturating_sub(
                self.timing
                    .seek_preroll_frames
                    .max(self.codec_preroll_frames),
            );
        let ts = frames_to_ticks(native_target, self.time_base, self.sample_rate);
        match self.reader.seek(
            SeekMode::Accurate,
            SeekTo::TimeStamp {
                ts,
                track_id: self.track_id,
            },
        ) {
            Ok(_) => {
                self.decoder.reset();
                self.samples.clear();
                self.sample_pos = 0;
                self.keep_from = target.saturating_add(self.timing.timestamp_offset_frames);
                self.next_frame = None;
                self.decode_errors = 0;
                self.finished = false;
                Ok(())
            }
            Err(error) => Err(SeekError::Other(Box::new(ContainerSeekError(error.to_string())))),
        }
    }
}

struct ContainerSeekError(String);

impl std::fmt::Debug for ContainerSeekError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::fmt::Display for ContainerSeekError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ContainerSeekError {}

fn duration_to_frames(duration: Duration, rate: u32) -> u64 {
    u64::try_from(duration.as_nanos().saturating_mul(u128::from(rate)) / 1_000_000_000)
        .unwrap_or(u64::MAX)
}

fn ticks_to_frames(ticks: u64, base: TimeBase, rate: u32) -> u64 {
    let numerator = u128::from(ticks)
        .saturating_mul(u128::from(base.numer))
        .saturating_mul(u128::from(rate));
    u64::try_from(numerator / u128::from(base.denom)).unwrap_or(u64::MAX)
}

fn frames_to_ticks(frames: u64, base: TimeBase, rate: u32) -> u64 {
    let numerator = u128::from(frames).saturating_mul(u128::from(base.denom));
    let denominator = u128::from(base.numer).saturating_mul(u128::from(rate));
    u64::try_from(numerator / denominator).unwrap_or(u64::MAX)
}

/// Open a selected-audio container while bounding the number of bytes the
/// demuxer can consume while probing. Playback callers can pass their preload
/// cancellation token; metadata callers without an owned token pass `None` and
/// check their borrowed token immediately before and after this synchronous call.
pub(crate) fn open_container<R: MediaSource + 'static>(
    source: R,
    kind: ContainerKind,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<OpenedAudio, String> {
    let probing = Arc::new(AtomicBool::new(true));
    let consumed = Arc::new(AtomicUsize::new(0));
    let source = ProbeLimitedSource {
        inner: source,
        probing: probing.clone(),
        consumed,
        cancel,
    };
    let stream = MediaSourceStream::new(Box::new(source), MediaSourceStreamOptions::default());
    let options = FormatOptions {
        enable_gapless: true,
        ..FormatOptions::default()
    };
    let (reader, track_id, codec_params, duration, duration_is_exact, timing) = match kind {
        ContainerKind::Mp4 => {
            let reader = IsoMp4Reader::try_new(stream, &options)
                .map_err(|error| container_open_error(error.to_string()))?;
            let (track_id, codec_params) = select_supported_audio_track(
                reader.default_track(),
                reader.tracks(),
            )
            .map(|track| (track.id, track.codec_params.clone()))
            .ok_or_else(|| "The container has no supported audio track".to_string())?;
            let container_timing = reader.audio_timing(track_id).unwrap_or_default();
            let duration = container_timing.duration;
            let duration_is_exact = container_timing.duration_is_exact;
            let timing = AudioTiming {
                timestamp_offset_frames: container_timing.timestamp_offset_frames,
                seek_preroll_frames: container_timing.seek_preroll_frames,
                timestamps_are_quantized: false,
            };
            (
                Box::new(reader) as Box<dyn FormatReader>,
                track_id,
                codec_params,
                duration,
                duration_is_exact,
                timing,
            )
        }
        ContainerKind::Matroska => {
            let reader = MkvReader::try_new(stream, &options)
                .map_err(|error| container_open_error(error.to_string()))?;
            let (track_id, codec_params) = select_supported_audio_track(
                reader.default_track(),
                reader.tracks(),
            )
            .map(|track| (track.id, track.codec_params.clone()))
            .ok_or_else(|| "The container has no supported audio track".to_string())?;
            let container_timing = reader.audio_timing(track_id).unwrap_or_default();
            let duration = container_timing.duration;
            let duration_is_exact = container_timing.duration_is_exact;
            let timing = AudioTiming {
                timestamp_offset_frames: container_timing.timestamp_offset_frames,
                seek_preroll_frames: container_timing.seek_preroll_frames,
                timestamps_are_quantized: true,
            };
            (
                Box::new(reader) as Box<dyn FormatReader>,
                track_id,
                codec_params,
                duration,
                duration_is_exact,
                timing,
            )
        }
    };
    probing.store(false, Ordering::Release);
    Ok(OpenedAudio {
        reader,
        track_id,
        codec_params,
        duration,
        duration_is_exact,
        timing,
    })
}

fn container_open_error(error: String) -> String {
    if error.contains("container probe byte limit exceeded") {
        "Container metadata exceeds the safe probe limit".to_string()
    } else if error.contains("cancelled") {
        "Container probing was cancelled".to_string()
    } else {
        error
    }
}

/// Keep the probe's input reads bounded and interruptible without changing the
/// format reader's normal later packet reads.
struct ProbeLimitedSource<R> {
    inner: R,
    probing: Arc<AtomicBool>,
    consumed: Arc<AtomicUsize>,
    cancel: Option<Arc<AtomicBool>>,
}

impl<R: Read> Read for ProbeLimitedSource<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.cancel.as_ref().is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return Err(io::Error::new(io::ErrorKind::Other, "cancelled"));
        }
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.probing.load(Ordering::Acquire) {
            let used = self.consumed.load(Ordering::Relaxed);
            let remaining = PROBE_READ_LIMIT.saturating_sub(used);
            if remaining == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "container probe byte limit exceeded",
                ));
            }
            let limit = buffer.len().min(remaining);
            let count = self.inner.read(&mut buffer[..limit])?;
            self.consumed.fetch_add(count, Ordering::Relaxed);
            Ok(count)
        } else {
            self.inner.read(buffer)
        }
    }
}

impl<R: Seek> Seek for ProbeLimitedSource<R> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        if self.cancel.as_ref().is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return Err(io::Error::new(io::ErrorKind::Other, "cancelled"));
        }
        self.inner.seek(position)
    }
}

impl<R: MediaSource> MediaSource for ProbeLimitedSource<R> {
    fn is_seekable(&self) -> bool {
        self.inner.is_seekable()
    }

    fn byte_len(&self) -> Option<u64> {
        self.inner.byte_len()
    }
}

const PROBE_READ_LIMIT: usize = 64 * 1024 * 1024;

/// The container parser normalizes Opus config to OpusHead before it reaches
/// this shared metadata/playback boundary.
fn opus_head(extra: &[u8]) -> Option<&[u8]> {
    (extra.len() >= 19 && extra.starts_with(b"OpusHead")).then_some(extra)
}
