//! Opus playback. Symphonia demuxes Ogg, MP4/M4A, and Matroska packets; the
//! pure-Rust `opuscule` decoder turns them into 48 kHz samples. The result is an
//! ordinary `Source`, so speed, effects, and gapless joins remain unchanged.
//!
//! Supported: mono and stereo (channel mapping family 0), seeking, the header's
//! output gain, the start pre-skip and the end padding. Not supported: surround
//! layouts (mapping families 1 and 255) and chained streams (the source ends at
//! the first chain boundary).
//!
//! Nothing here may panic on a damaged file: every library call runs inside
//! `catch_unwind`, every buffer is bounded by the Opus frame limit or a fixed
//! window, and a failure ends the source instead of the audio thread.
use opuscule::{Channels, Decoder, SampleRate};
use rodio::{Source, source::SeekError};
use std::{
    fmt,
    fs::File,
    io::{Read, Seek, SeekFrom},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use symphonia::{
    core::{
        codecs::CODEC_TYPE_OPUS,
        errors::Error as DemuxError,
        formats::{FormatOptions, FormatReader, Packet, SeekMode, SeekTo},
        io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions},
        units::TimeBase,
    },
    default::formats::OggReader,
};

const OUTPUT_RATE: u32 = 48_000;
/// The longest frame Opus can produce (120 ms) in samples per channel.
const MAX_FRAME: usize = 5760;
/// Decoder warm-up before a seek target (RFC 7845 §4.6 recommends 80 ms).
const PRE_ROLL_FRAMES: u64 = 3840;
/// The identification header is read from the first page; anything longer than
/// this cannot be a valid one (the largest has 19 + 2 + 255 bytes).
const HEAD_READ_MAX: usize = 512;

/// Header output gain (Q7.8 dB, RFC 7845 §5.1) is applied within this range.
const GAIN_MIN_DB: f32 = -48.;
const GAIN_MAX_DB: f32 = 24.;
/// A hole in the page timeline smaller than this (2.5 ms) is not a loss.
const GAP_MIN_FRAMES: u64 = 120;
/// Lost audio is replaced by silence up to this much (5 s); a larger jump of the
/// timeline is taken as a damaged timestamp and ignored.
const GAP_MAX_FRAMES: u64 = 5 * OUTPUT_RATE as u64;
/// Consecutive unreadable packets or pages before the source gives up.
const MAX_CONSECUTIVE_ERRORS: u32 = 64;
/// Unsupported/empty packets drained without producing audio before the source
/// fails explicitly instead of monopolizing one iterator refill.
const MAX_CONSECUTIVE_SKIPS: u32 = 256;
/// Packets drained while refilling one output block, including multiplexed
/// tracks and packets whose decoded samples are fully trimmed.
const MAX_REFILL_STEPS: usize = 4096;
/// Lengths above this (1000 hours) are not believed.
const MAX_TOTAL_FRAMES: u64 = OUTPUT_RATE as u64 * 3600 * 1000;

const MSG_NOT_OPUS: &str = "This file is not an Ogg Opus stream";
const MSG_LAYOUT: &str = "This Opus file uses a channel layout that is not supported yet";
const MSG_HEADER: &str = "This Opus file has a damaged header";
const MSG_PANIC: &str = "The Opus decoder failed unexpectedly on this file";
const MSG_UNUSABLE_PACKETS: &str = "This Ogg Opus stream has too many packets that this build cannot play";



/// Runs a library call so that a panic in it ends only this source.
fn guarded<T>(call: impl FnOnce() -> T) -> Option<T> {
    match catch_unwind(AssertUnwindSafe(call)) {
        Ok(value) => Some(value),
        Err(_) => {

            None
        }
    }
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at.checked_add(2)?)?.try_into().ok()?))
}
fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at.checked_add(4)?)?.try_into().ok()?))
}
fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut value = (i as u32) << 24;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 0x8000_0000 != 0 {
                (value << 1) ^ 0x04C1_1DB7
            } else {
                value << 1
            };
            bit += 1;
        }
        table[i] = value;
        i += 1;
    }
    table
}
static CRC_TABLE: [u32; 256] = crc_table();

/// The Ogg page checksum (CRC-32, polynomial 0x04C11DB7, no reflection) of a page
/// whose checksum field (bytes 22..26) is taken as zero.
fn page_crc(page: &[u8]) -> u32 {
    let mut crc = 0u32;
    for (index, &byte) in page.iter().enumerate() {
        let byte = if (22..26).contains(&index) { 0 } else { byte };
        let slot = usize::from((crc >> 24) as u8 ^ byte);
        crc = (crc << 8) ^ CRC_TABLE.get(slot).copied().unwrap_or(0);
    }
    crc
}

/// A page whose header and checksum are valid.
struct PageInfo {
    granule: u64,
    serial: u32,
    flags: u8,
}

/// Checks that `bytes` starts with one complete, intact Ogg page.
fn check_page(bytes: &[u8]) -> Option<PageInfo> {
    if bytes.get(..4)? != b"OggS" || *bytes.get(4)? != 0 {
        return None;
    }
    let segments = usize::from(*bytes.get(26)?);
    let lacing = bytes.get(27..27 + segments)?;
    let body: usize = lacing.iter().map(|&l| usize::from(l)).sum();
    let page = bytes.get(..27 + segments + body)?;
    if page_crc(page) != u32_at(page, 22)? {
        return None;
    }
    Some(PageInfo {
        granule: u64_at(page, 6)?,
        serial: u32_at(page, 14)?,
        flags: *page.get(5)?,
    })
}

/// Consume a bounded page body without retaining it or decoding audio.
fn skip_page_body<R: Read>(reader: &mut R, mut length: usize) -> bool {
    let mut chunk = [0u8; 8 * 1024];
    while length > 0 {
        let count = length.min(chunk.len());
        if reader.read_exact(&mut chunk[..count]).is_err() {
            return false;
        }
        length -= count;
    }
    true
}

/// Find the first logical stream's end, including when later chains reuse its
/// serial. Check page framing and the terminal EOS checksum; a damaged middle
/// payload does not erase a valid end trim. Reads the first stream without audio
/// decoding, retaining at most one maximum-size Ogg page.
fn first_stream_granule<R: Read + Seek>(
    reader: &mut R,
    serial: u32,
    cancel: Option<&AtomicBool>,
) -> Option<u64> {
    reader.seek(SeekFrom::Start(0)).ok()?;
    let mut first = true;
    loop {
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
            return None;
        }
        let mut prefix = [0u8; 4];
        let mut filled = 0;
        while filled < prefix.len() {
            match reader.read(&mut prefix[filled..]) {
                Ok(0) if filled == 0 => return None,
                Ok(0) => return None,
                Ok(count) => filled += count,
                Err(_) => return None,
            }
        }
        if &prefix != b"OggS" {
            return None;
        }
        let mut header = [0u8; 27];
        header[..4].copy_from_slice(&prefix);
        if reader.read_exact(&mut header[4..]).is_err() {
            return None;
        }
        if header[4] != 0 {
            return None;
        }
        let segments = usize::from(header[26]);
        let mut lacing = vec![0u8; segments];
        if reader.read_exact(&mut lacing).is_err() {
            return None;
        }
        let body_len: usize = lacing.iter().map(|&size| usize::from(size)).sum();
        let page_serial = u32_at(&header, 14)?;
        if page_serial != serial {
            return None;
        }
        if first {
            if header[5] & 0x02 == 0 {
                return None;
            }
            first = false;
        } else if header[5] & 0x02 != 0 {
            // A second BOS before this serial's EOS is malformed/interleaved.
            return None;
        }
        if header[5] & 0x04 != 0 {
            let mut page = Vec::with_capacity(27 + segments + body_len);
            page.extend_from_slice(&header);
            page.extend_from_slice(&lacing);
            let body_start = page.len();
            page.resize(body_start + body_len, 0);
            if reader.read_exact(&mut page[body_start..]).is_err() {
                return None;
            }
            let info = check_page(&page)?;
            if info.serial != serial || info.flags & 0x04 == 0 {
                return None;
            }
            return (info.granule != u64::MAX).then_some(info.granule);
        }
        // Nonterminal audio bodies may have a bad CRC and still be skipped by
        // the decoder. Their page header/lacing establish the next boundary;
        // the terminal EOS page is fully read and CRC-checked above.
        if !skip_page_body(reader, body_len) {
            return None;
        }
    }
}

/// The first packet of the first Ogg page (at most `HEAD_READ_MAX` bytes of it)
/// and the page's stream serial. The position is left wherever the read ended.
fn read_first_packet<R: Read + Seek>(reader: &mut R) -> Option<(Vec<u8>, u32)> {
    reader.seek(SeekFrom::Start(0)).ok()?;
    let mut header = [0u8; 27];
    reader.read_exact(&mut header).ok()?;
    if &header[..4] != b"OggS" || header[4] != 0 {
        return None;
    }
    let serial = u32_at(&header, 14)?;
    let segments = usize::from(header[26]);
    let mut lacing = [0u8; 255];
    reader.read_exact(lacing.get_mut(..segments)?).ok()?;
    let mut length = 0usize;
    for &value in lacing.get(..segments)? {
        length += usize::from(value);
        if value < 255 {
            break;
        }
    }
    let mut packet = vec![0u8; length.min(HEAD_READ_MAX)];
    reader.read_exact(&mut packet).ok()?;
    Some((packet, serial))
}

/// True when the file is an Ogg stream whose first packet is an Opus
/// identification header. Looks at the first page only and puts the file position
/// back where it was.
pub fn is_ogg_opus(file: &mut File) -> bool {
    let at = file.stream_position().ok();
    let opus = read_first_packet(file).is_some_and(|(packet, _)| packet.starts_with(b"OpusHead"));
    if let Some(at) = at {
        let _ = file.seek(SeekFrom::Start(at));
    }
    opus
}

/// What the player needs from the identification header (RFC 7845 §5.1).
struct OpusHead {
    channels: Channels,
    pre_skip: u16,
    gain_q8: i16,
}

fn parse_head(packet: &[u8]) -> Result<OpusHead, String> {
    if !packet.starts_with(b"OpusHead") {
        return Err(MSG_NOT_OPUS.into());
    }
    let (Some(&version), Some(&channels), Some(pre_skip), Some(gain), Some(&family)) = (
        packet.get(8),
        packet.get(9),
        u16_at(packet, 10),
        u16_at(packet, 16),
        packet.get(18),
    ) else {
        return Err(MSG_HEADER.into());
    };
    if version > 0x0f || channels == 0 {
        return Err(MSG_HEADER.into());
    }
    if family != 0 {
        return Err(MSG_LAYOUT.into());
    }
    let channels = Channels::from_count(usize::from(channels)).ok_or(MSG_LAYOUT)?;
    Ok(OpusHead {
        channels,
        pre_skip,
        gain_q8: gain as i16,
    })
}

/// Linear factor for the header's output gain (Q7.8 dB). The range is limited so a
/// damaged header cannot make the track silent or deafening.
fn output_gain(gain_q8: i16) -> f32 {
    let db = (f32::from(gain_q8) / 256.).clamp(GAIN_MIN_DB, GAIN_MAX_DB);
    10f32.powf(db / 20.)
}

fn frames_to_duration(frames: u64) -> Duration {
    let rate = u64::from(OUTPUT_RATE);
    let nanos = (frames % rate) * 1_000_000_000 / rate;
    Duration::new(frames / rate, nanos as u32)
}

fn duration_to_frames(duration: Duration) -> u64 {
    let frames = duration.as_nanos().saturating_mul(u128::from(OUTPUT_RATE)) / 1_000_000_000;
    u64::try_from(frames).unwrap_or(u64::MAX)
}

/// Convert native packet ticks into output-rate sample frames without going
/// through floating point. Containers with rounded timestamps use this only to
/// establish a packet's initial position; packet order then advances the exact
/// decoded frame count.
fn timestamp_to_frames(ticks: u64, time_base: TimeBase, rate: u32) -> u64 {
    let numerator = u128::from(ticks)
        .saturating_mul(u128::from(time_base.numer))
        .saturating_mul(u128::from(rate));
    u64::try_from(numerator / u128::from(time_base.denom)).unwrap_or(u64::MAX)
}

/// Convert output-rate sample frames into native timestamp ticks for demuxer
/// seeking. The demuxer then selects a packet before the target and the source
/// decodes forward to the requested frame.
fn frames_to_timestamp(frames: u64, time_base: TimeBase, rate: u32) -> u64 {
    let numerator = u128::from(frames).saturating_mul(u128::from(time_base.denom));
    let denominator = u128::from(time_base.numer).saturating_mul(u128::from(rate));
    u64::try_from(numerator / denominator).unwrap_or(u64::MAX)
}


struct OpusSeekError(String);
impl fmt::Debug for OpusSeekError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl fmt::Display for OpusSeekError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for OpusSeekError {}

/// What the demuxer returned for one read.
enum Next {
    Packet(Packet),
    /// Nothing usable this time (a damaged page was skipped): ask again.
    Again,
    End,
}

/// A decoded Opus stream as a rodio source: 48 kHz, one or two channels.
pub struct OpusSource {
    reader: Box<dyn FormatReader>,
    track_id: u32,
    /// Granule position of the first packet; the stream's time zero.
    start_ts: u64,
    time_base: TimeBase,
    /// Container Opus uses packet trim fields and a presentation coordinate
    /// offset. Ogg keeps its existing granule/pre-skip path.
    container_timing: Option<crate::container_audio::AudioTiming>,
    decoder: Box<Decoder>,
    channels: usize,
    pre_skip: u64,
    gain: f32,
    /// Length in frames after the pre-skip, from the last page's granule position.
    total_frames: Option<u64>,
    /// Granule index (relative to `start_ts`) where the audio ends.
    end_granule: Option<u64>,
    /// One decoded frame block, interleaved; `pos..len` is what is not played yet.
    pcm: Vec<f32>,
    pos: usize,
    len: usize,
    /// Frames before this granule index are decoded and thrown away (the pre-skip
    /// at the start, the warm-up after a seek).
    keep_from: u64,
    /// Granule index of the next frame the decoder will produce.
    next_granule: Option<u64>,
    /// Silent frames still owed for a hole in the page timeline.
    gap: u64,
    /// The packet that revealed that hole, decoded after the silence.
    held: Option<Packet>,
    finished: bool,
    demux_errors: u32,
    decode_errors: u32,
    skipped_packets: u32,
    /// Why the source ended early, if it did (diagnostics and tests).
    #[allow(dead_code)]
    error: Option<String>,
}

impl OpusSource {
    /// Opens an Ogg Opus stream. Fails with a message fit for the user.
    pub fn from_media<R: MediaSource + 'static>(media: R) -> Result<Self, String> {
        Self::from_media_with_cancel(media, None)
    }

    fn from_media_with_cancel<R: MediaSource + 'static>(
        mut media: R,
        cancel: Option<&AtomicBool>,
    ) -> Result<Self, String> {
        let plan = guarded(|| Self::open(&mut media, cancel))
            .ok_or_else(|| MSG_PANIC.to_string())??;
        Self::start(plan, media)
    }

    /// Everything that can be learned before the demuxer takes over the stream.
    fn open<R: MediaSource>(media: &mut R, cancel: Option<&AtomicBool>) -> Result<Plan, String> {
        let (packet, serial) = read_first_packet(media).ok_or(MSG_NOT_OPUS)?;
        let head = parse_head(&packet)?;
        let last = first_stream_granule(media, serial, cancel);
        media.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        Ok(Plan { head, serial, last })
    }

    fn start<R: MediaSource + 'static>(plan: Plan, media: R) -> Result<Self, String> {
        let stream = MediaSourceStream::new(Box::new(media), MediaSourceStreamOptions::default());
        let options = FormatOptions {
            enable_gapless: false,
            ..FormatOptions::default()
        };
        let reader = guarded(|| OggReader::try_new(stream, &options))
            .ok_or_else(|| MSG_PANIC.to_string())?
            .map_err(|e| e.to_string())?;
        let (track_id, start_ts, time_base) = reader
            .tracks()
            .iter()
            .find(|track| track.codec_params.codec == CODEC_TYPE_OPUS)
            .map(|track| {
                (
                    track.id,
                    track.codec_params.start_ts,
                    track
                        .codec_params
                        .time_base
                        .unwrap_or_else(|| TimeBase::new(1, OUTPUT_RATE)),
                )
            })
            .ok_or(MSG_HEADER)?;
        if track_id != plan.serial {
            return Err(MSG_HEADER.into());
        }
        let pre_skip = u64::from(plan.head.pre_skip);
        let total_frames = plan
            .last
            .map(|granule| granule.saturating_sub(start_ts).saturating_sub(pre_skip))
            .filter(|&frames| frames <= MAX_TOTAL_FRAMES);
        let channels = plan.head.channels.count();
        Ok(Self {
            reader: Box::new(reader),
            track_id,
            start_ts,
            time_base,
            container_timing: None,
            decoder: Box::new(Decoder::new(SampleRate::Hz48000, plan.head.channels)),
            channels,
            pre_skip,
            gain: output_gain(plan.head.gain_q8),
            total_frames,
            end_granule: total_frames.map(|frames| frames + pre_skip),
            pcm: vec![0.; MAX_FRAME * channels],
            pos: 0,
            len: 0,
            keep_from: pre_skip,
            next_granule: None,
            gap: 0,
            held: None,
            finished: false,
            demux_errors: 0,
            decode_errors: 0,
            skipped_packets: 0,
            error: None,
        })
    }

    /// Builds the Opus source for the selected MP4/Matroska track. The parser
    /// normalizes container configuration to OpusHead, and its packet trims
    /// describe decoded-frame cuts exactly once; do not apply OpusHead pre-skip
    /// again on this path.
    pub(crate) fn from_container(
        opened: crate::container_audio::OpenedAudio,
    ) -> Result<Self, String> {
        if opened.codec_params.codec != CODEC_TYPE_OPUS {
            return Err("The selected container track is not Opus".into());
        }
        let extra = opened
            .codec_params
            .extra_data
            .as_deref()
            .ok_or(MSG_HEADER)?;
        let head = parse_head(extra)?;
        let time_base = opened
            .codec_params
            .time_base
            .ok_or_else(|| "The Opus track has no timestamp time base".to_string())?;
        let duration = opened.duration.map(duration_to_frames);
        let duration = duration.filter(|&frames| frames <= MAX_TOTAL_FRAMES);
        let exact_end = if opened.duration_is_exact {
            duration
                .filter(|&frames| frames <= MAX_TOTAL_FRAMES)
                .map(|frames| frames.saturating_add(opened.timing.timestamp_offset_frames))
        } else {
            None
        };
        let channels = head.channels.count();
        let (track_id, reader) = (opened.track_id, opened.reader);
        Ok(Self {
            reader,
            track_id,
            start_ts: opened.codec_params.start_ts,
            time_base,
            container_timing: Some(opened.timing),
            decoder: Box::new(Decoder::new(SampleRate::Hz48000, head.channels)),
            channels,
            pre_skip: 0,
            gain: output_gain(head.gain_q8),
            total_frames: duration,
            end_granule: exact_end,
            pcm: vec![0.; MAX_FRAME * channels],
            pos: 0,
            len: 0,
            keep_from: opened.timing.timestamp_offset_frames,
            next_granule: None,
            gap: 0,
            held: None,
            finished: false,
            demux_errors: 0,
            decode_errors: 0,
            skipped_packets: 0,
            error: None,
        })
    }


    fn fail(&mut self, message: &str) {
        if self.error.is_none() {
            self.error = Some(message.to_string());
        }
        self.finished = true;
    }

    fn note_unusable_packet(&mut self) {
        self.skipped_packets = self.skipped_packets.saturating_add(1);
        if self.skipped_packets > MAX_CONSECUTIVE_SKIPS {
            self.fail(MSG_UNUSABLE_PACKETS);
        }
    }

    fn next_packet(&mut self) -> Next {
        let reader = &mut self.reader;
        match guarded(|| reader.next_packet()) {
            None => {
                self.fail(MSG_PANIC);
                Next::End
            }
            Some(Ok(packet)) => {
                self.demux_errors = 0;
                Next::Packet(packet)
            }
            Some(Err(DemuxError::IoError(error))) => {
                // The end of the file is the normal end; anything else is remembered.
                if error.kind() != std::io::ErrorKind::UnexpectedEof {
                    self.fail(&error.to_string());
                }
                Next::End
            }
            // A new stream begins here (a chained file): this one is over.
            Some(Err(DemuxError::ResetRequired)) => Next::End,
            Some(Err(DemuxError::DecodeError(_))) => {
                self.demux_errors = self.demux_errors.saturating_add(1);
                if self.demux_errors > MAX_CONSECUTIVE_ERRORS {
                    self.fail("The Opus stream is damaged");
                    Next::End
                } else {
                    Next::Again
                }
            }
            Some(Err(other)) => {
                self.fail(&other.to_string());
                Next::End
            }
        }
    }

    /// Produces one block of frames (silence for a lost stretch or an unreadable
    /// packet, decoded audio otherwise) and hands it to `emit`.
    fn step(&mut self) {
        if self.gap > 0 {
            let frames = self.gap.min(MAX_FRAME as u64) as usize;
            self.gap -= frames as u64;
            self.silence(frames);
            return;
        }
        let packet = match self.held.take() {
            Some(packet) => packet,
            None => {
                let packet = match self.next_packet() {
                    Next::Packet(packet) => packet,
                    Next::Again => {
                        self.note_unusable_packet();
                        return;
                    }
                    Next::End => {
                        self.finished = true;
                        return;
                    }
                };
                if packet.track_id() != self.track_id {
                    // Interleaved video and subtitle packets are not Opus errors.
                    return;
                }
                if packet.data.is_empty() {
                    self.note_unusable_packet();
                    return;
                }
                let granule = self.packet_start(&packet);
                match self.next_granule {
                    None => self.next_granule = Some(granule),
                    Some(_) if self.timestamps_are_quantized() => {}
                    Some(next)
                        if granule > next.saturating_add(GAP_MIN_FRAMES)
                            && granule - next <= GAP_MAX_FRAMES =>
                    {
                        // Pages are missing: keep the timeline with silence.
                        self.gap = granule - next;
                        self.held = Some(packet);
                        return;
                    }
                    // A packet that starts before the expected position is the
                    // last page's end padding (its timestamps count back from the
                    // final granule position); nothing is lost.
                    Some(_) => {}
                }
                packet
            }
        };
        self.decode(&packet);
    }

    fn silence(&mut self, frames: usize) {
        if let Some(block) = self.pcm.get_mut(..frames * self.channels) {
            block.fill(0.);
        }
        self.emit(frames, 0, 0);
    }

    fn timestamps_are_quantized(&self) -> bool {
        self.container_timing
            .is_some_and(|timing| timing.timestamps_are_quantized)
    }

    fn packet_start(&self, packet: &Packet) -> u64 {
        if self.container_timing.is_none() {
            return packet.ts.saturating_sub(self.start_ts);
        }
        timestamp_to_frames(packet.ts, self.time_base, OUTPUT_RATE)
    }

    fn decode(&mut self, packet: &Packet) {
        let decoder = &mut self.decoder;
        let pcm = self.pcm.as_mut_slice();
        let data: &[u8] = &packet.data;
        let (trim_start, trim_end) = if self.container_timing.is_some() {
            (packet.trim_start as usize, packet.trim_end as usize)
        } else {
            (0, 0)
        };
        match guarded(|| decoder.decode(Some(data), pcm, false)) {
            None => self.fail(MSG_PANIC),
            Some(Ok(frames)) if frames <= MAX_FRAME => {
                self.decode_errors = 0;
                if frames == 0 {
                    self.note_unusable_packet();
                } else {
                    self.skipped_packets = 0;
                }
                self.emit(frames, trim_start, trim_end);
            }
            Some(_) => {
                // A packet the decoder refuses: one packet's worth of silence.
                self.decode_errors = self.decode_errors.saturating_add(1);
                if self.decode_errors > MAX_CONSECUTIVE_ERRORS {
                    self.fail("The Opus stream is damaged");
                    return;
                }
                self.skipped_packets = 0;
                let length = if self.container_timing.is_some() {
                    usize::try_from(timestamp_to_frames(packet.dur, self.time_base, OUTPUT_RATE))
                        .unwrap_or(0)
                } else {
                    usize::try_from(packet.dur).unwrap_or(0)
                };
                let frames = if length == 0 { 960 } else { length.min(MAX_FRAME) };
                self.silence_trimmed(frames, trim_start, trim_end);
            }
        }
    }

    fn silence_trimmed(&mut self, frames: usize, trim_start: usize, trim_end: usize) {
        if let Some(block) = self.pcm.get_mut(..frames * self.channels) {
            block.fill(0.);
        }
        self.emit(frames, trim_start, trim_end);
    }

    /// Takes the decoded frames: intersects the container presentation interval
    /// with packet trim_start/trim_end once (or keeps the legacy Ogg granule
    /// pre-skip/end-granule behavior), applies gain, and exposes the remainder.
    fn emit(&mut self, frames: usize, trim_start: usize, trim_end: usize) {
        let channels = self.channels;
        let start = self.next_granule.unwrap_or(0);
        let block_end = start.saturating_add(frames as u64);
        self.next_granule = Some(block_end);
        let from = (self.keep_from.saturating_sub(start).min(frames as u64) as usize)
            .max(trim_start.min(frames));
        let mut to = frames.saturating_sub(trim_end.min(frames));
        if let Some(end) = self.end_granule {
            to = to.min(end.saturating_sub(start).min(frames as u64) as usize);
            if block_end >= end {
                self.finished = true;
            }
        }
        if from >= to {
            return;
        }
        let gain = self.gain;
        if let Some(block) = self.pcm.get_mut(from * channels..to * channels) {
            for sample in block {
                // The decoder never yields NaN or infinity for a valid stream; a
                // damaged one must not poison the filters behind this source.
                *sample = if sample.is_finite() { *sample * gain } else { 0. };
            }
            self.pos = from * channels;
            self.len = to * channels;
        }
    }

    /// Decodes until some audio is available or the stream is over.
    fn refill(&mut self) -> bool {
        self.pos = 0;
        self.len = 0;
        let mut steps = 0;
        while self.len == 0 {
            if self.finished {
                return false;
            }
            steps += 1;
            if steps > MAX_REFILL_STEPS {
                self.fail(MSG_UNUSABLE_PACKETS);
                return false;
            }
            self.step();
        }
        true
    }
}

/// What `open` learned from the file head and tail.
struct Plan {
    head: OpusHead,
    serial: u32,
    last: Option<u64>,
}

impl Iterator for OpusSource {
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<f32> {
        if self.pos >= self.len && !self.refill() {
            return None;
        }
        let sample = self.pcm.get(self.pos).copied();
        self.pos += 1;
        sample
    }
}

impl Source for OpusSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> u16 {
        self.channels as u16
    }

    fn sample_rate(&self) -> u32 {
        OUTPUT_RATE
    }

    fn total_duration(&self) -> Option<Duration> {
        self.total_frames.map(frames_to_duration)
    }

    /// Seeks to `position` (clamped to the end): the demuxer finds the page 80 ms
    /// ahead, the decoder is restarted and the warm-up is decoded and dropped on
    /// the way to the target. Within the last Ogg page the position can be off by
    /// the file's end padding (less than one packet).
    fn try_seek(&mut self, position: Duration) -> Result<(), SeekError> {
        let mut target = duration_to_frames(position);
        if self.end_granule.is_some() {
            if let Some(total) = self.total_frames {
                target = target.min(total);
            }
        }
        let container_timing = self.container_timing;
        let target_granule = if container_timing.is_some() {
            target.saturating_add(
                container_timing.map_or(0, |timing| timing.timestamp_offset_frames),
            )
        } else {
            self.pre_skip.saturating_add(target)
        };
        let preroll = container_timing
            .map(|timing| timing.seek_preroll_frames)
            .filter(|&frames| frames > 0)
            .unwrap_or(PRE_ROLL_FRAMES);
        let ts = if let Some(timing) = container_timing {
            let raw_target = target.saturating_add(timing.timestamp_offset_frames);
            frames_to_timestamp(
                raw_target.saturating_sub(preroll),
                self.time_base,
                OUTPUT_RATE,
            )
        } else {
            self.start_ts
                .saturating_add(target_granule.saturating_sub(preroll))
        };
        let (reader, track_id) = (&mut self.reader, self.track_id);
        let seek = SeekTo::TimeStamp { ts, track_id };
        let failure = match guarded(|| reader.seek(SeekMode::Accurate, seek)) {
            None => Some(MSG_PANIC.to_string()),
            Some(Err(error)) => Some(error.to_string()),
            Some(Ok(_)) => None,
        };
        if let Some(message) = failure {
            // The reader may stand anywhere now: end rather than play from there.
            self.fail(&message);
            return Err(SeekError::Other(Box::new(OpusSeekError(message))));
        }
        let channels = if self.channels == 1 {
            Channels::Mono
        } else {
            Channels::Stereo
        };
        self.decoder = Box::new(Decoder::new(SampleRate::Hz48000, channels));
        self.pos = 0;
        self.len = 0;
        self.keep_from = target_granule;
        self.next_granule = None;
        self.gap = 0;
        self.held = None;
        self.finished = false;
        self.demux_errors = 0;
        self.decode_errors = 0;
        self.skipped_packets = 0;
        self.error = None;
        Ok(())
    }
}

/// Opens an Ogg Opus file as a source. Call `is_ogg_opus` first.
pub fn open_opus(file: File) -> Result<Box<dyn Source + Send>, String> {
    Ok(Box::new(OpusSource::from_media(file)?))
}

/// Opens the selected Opus track from an MP4 or Matroska container.
pub(crate) fn open_container_opus(
    opened: crate::container_audio::OpenedAudio,
) -> Result<Box<dyn Source + Send>, String> {
    Ok(Box::new(OpusSource::from_container(opened)?))
}

/// Opens a preloaded Ogg Opus source, allowing its page scan to stop when the
/// preload is withdrawn.
pub fn open_opus_cancelable(
    file: File,
    cancel: &AtomicBool,
) -> Result<Box<dyn Source + Send>, String> {
    Ok(Box::new(OpusSource::from_media_with_cancel(
        file,
        Some(cancel),
    )?))
}
