//! Tag-independent audio identity and packet-derived duration.

use super::safety::{
    MAX_OGG_HEADER_PACKET_BYTES, identity_container, validate_lofty_input_cancellable,
    validate_ogg_audio_packet_limits,
};
use ring::digest::{Context as Sha256Context, SHA256};
use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use symphonia::core::{
    codecs::CODEC_TYPE_NULL,
    errors::Error as SymphoniaError,
    formats::FormatOptions,
    io::{MediaSourceStream, MediaSourceStreamOptions},
    meta::MetadataOptions,
    probe::Hint,
};

/// A tag-independent digest of demuxed audio packets for formats with reliable
/// packet boundaries. Other formats remain unknown and cannot prove a retag.
pub(super) struct AudioIdentity {
    pub(super) digest: String,
    pub(super) duration: Option<f64>,
    pub(super) mp3: bool,
}

pub(super) fn audio_content_identity_cancellable(
    path: &Path,
    cancel: &AtomicBool,
) -> Option<AudioIdentity> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let extension = identity_container(path, cancel).ok()??;
        validate_lofty_input_cancellable(path, Some(cancel)).ok()?;
        validate_ogg_audio_packet_limits(path, cancel).ok()?;
        if cancel.load(AtomicOrdering::Relaxed) {
            return None;
        }
        let source = File::open(path).ok()?;
        let stream = MediaSourceStream::new(Box::new(source), MediaSourceStreamOptions::default());
        let mut hint = Hint::new();
        hint.with_extension(extension);
        let options = FormatOptions {
            enable_gapless: false,
            ..FormatOptions::default()
        };
        let mut probe = symphonia::core::probe::Probe::default();
        probe.register_all::<symphonia::default::formats::MpaReader>();
        probe.register_all::<symphonia::default::formats::FlacReader>();
        if extension == "ogg" {
            probe.register_all::<symphonia::default::formats::OggReader>();
        }
        probe.register_all::<symphonia_metadata::id3v2::Id3v2Reader>();
        let mut reader = probe
            .format(&hint, stream, &options, &MetadataOptions::default())
            .ok()?
            .format;
        let tracks: Vec<_> = reader
            .tracks()
            .iter()
            .filter(|track| track.codec_params.codec != CODEC_TYPE_NULL)
            .collect();
        let [track] = tracks.as_slice() else {
            return None;
        };
        let track_id = track.id;
        let params = &track.codec_params;
        let time_base = params.time_base;
        let sample_rate = params.sample_rate;
        let delay = params.delay;
        let padding = params.padding;
        let mut hash = Sha256Context::new(&SHA256);
        hash.update(b"bs-player-audio-packets-v1\0");
        hash.update(format!("{:?}", params.codec).as_bytes());
        update_optional_u32(&mut hash, params.sample_rate);
        hash.update(&params.start_ts.to_le_bytes());
        update_optional_u32(&mut hash, params.bits_per_sample);
        update_optional_u32(&mut hash, params.bits_per_coded_sample);
        update_optional_u32(&mut hash, params.delay);
        update_optional_u32(&mut hash, params.padding);
        update_optional_u64(&mut hash, params.max_frames_per_packet);
        hash.update(format!("{:?}", params.sample_format).as_bytes());
        hash.update(format!("{:?}", params.channels).as_bytes());
        hash.update(format!("{:?}", params.channel_layout).as_bytes());
        if let Some(time_base) = params.time_base {
            hash.update(&[1]);
            hash.update(&time_base.numer.to_le_bytes());
            hash.update(&time_base.denom.to_le_bytes());
        } else {
            hash.update(&[0]);
        }
        if let Some(extra_data) = params.extra_data.as_deref() {
            hash.update(&(extra_data.len() as u64).to_le_bytes());
            hash.update(extra_data);
        } else {
            hash.update(&0u64.to_le_bytes());
        }

        let mut packet_count = 0u64;
        let mut duration_ticks = 0u128;
        loop {
            if cancel.load(AtomicOrdering::Relaxed) {
                return None;
            }
            let packet = match reader.next_packet() {
                Ok(packet) => packet,
                Err(SymphoniaError::IoError(error))
                    if error.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    break;
                }
                // Chained streams, damaged pages, and read errors are ambiguous.
                Err(_) => return None,
            };
            if packet.track_id() != track_id || packet.data.len() > MAX_OGG_HEADER_PACKET_BYTES {
                return None;
            }
            hash.update(&(packet.data.len() as u64).to_le_bytes());
            hash.update(&packet.ts().to_le_bytes());
            hash.update(&packet.dur().to_le_bytes());
            hash.update(&packet.trim_start.to_le_bytes());
            hash.update(&packet.trim_end.to_le_bytes());
            hash.update(&packet.data);
            packet_count = packet_count.checked_add(1)?;
            duration_ticks = duration_ticks.checked_add(u128::from(packet.dur()))?;
        }
        if packet_count == 0 || duration_ticks == 0 {
            return None;
        }
        hash.update(&packet_count.to_le_bytes());
        hash.update(&duration_ticks.to_le_bytes());
        let digest = hash.finish();
        let duration = time_base.and_then(|base| {
            let total_seconds =
                duration_ticks as f64 * f64::from(base.numer) / f64::from(base.denom);
            let sample_rate = sample_rate?;
            let trim_samples =
                u64::from(delay.unwrap_or(0)).saturating_add(u64::from(padding.unwrap_or(0)));
            let audible = total_seconds - trim_samples as f64 / f64::from(sample_rate);
            (audible.is_finite() && audible > 0.).then_some(audible)
        });
        Some(AudioIdentity {
            digest: digest
                .as_ref()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            duration,
            mp3: extension == "mp3",
        })
    }))
    .ok()
    .flatten()
}

fn update_optional_u32(hash: &mut Sha256Context, value: Option<u32>) {
    match value {
        Some(value) => {
            hash.update(&[1]);
            hash.update(&value.to_le_bytes());
        }
        None => hash.update(&[0]),
    }
}

fn update_optional_u64(hash: &mut Sha256Context, value: Option<u64>) {
    match value {
        Some(value) => {
            hash.update(&[1]);
            hash.update(&value.to_le_bytes());
        }
        None => hash.update(&[0]),
    }
}
