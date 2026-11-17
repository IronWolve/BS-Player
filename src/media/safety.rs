//! Bounded preflight checks for untrusted media tag headers.

use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

/// Bound Ogg header packets before Lofty reads comments or allocates tag items.
pub(in crate::media) const MAX_OGG_HEADER_PACKET_BYTES: usize = 16 * 1024 * 1024;
const MAX_OGG_CODEC_HEADER_BYTES: usize = 64 * 1024;
const MAX_OGG_COMMENT_PACKET_BYTES: usize = 1024 * 1024;
const MAX_OGG_COMMENT_ITEMS: usize = 4096;
const MAX_ID3_TAG_BYTES: u32 = 16 * 1024 * 1024;
const MAX_ID3_TOTAL_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ID3_TAGS: usize = 16;
const MAX_FLAC_METADATA_BYTES: u64 = 32 * 1024 * 1024;
const MAX_FLAC_METADATA_BLOCKS: usize = 128;
/// Checks bounded ID3, Ogg, and FLAC tag metadata before calling Lofty.
/// Compressed ID3 frames are disabled in the Lofty dependency features;
/// comment readers need explicit bounds before reserving comment items.
pub fn validate_lofty_input(path: &Path) -> Result<(), String> {
    validate_lofty_input_cancellable(path, None)
}

pub(super) fn validate_lofty_input_cancellable(
    path: &Path,
    cancel: Option<&AtomicBool>,
) -> Result<(), String> {
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let (is_ogg, _) = skip_id3_prefixes(&mut file, cancel)?;
    if is_ogg {
        return validate_ogg_headers(&mut file, cancel, false);
    }
    let marker_start = file.stream_position().map_err(|error| error.to_string())?;
    let mut marker = [0u8; 4];
    if read_prefix(&mut file, &mut marker).map_err(|error| error.to_string())? != marker.len() {
        return Ok(());
    }
    file.seek(std::io::SeekFrom::Start(marker_start))
        .map_err(|error| error.to_string())?;
    if &marker == b"fLaC" {
        return validate_flac_metadata(&mut file, cancel);
    }
    Ok(())
}

fn validate_flac_metadata(file: &mut File, cancel: Option<&AtomicBool>) -> Result<(), String> {
    let file_len = file.metadata().map_err(|error| error.to_string())?.len();
    let mut marker = [0u8; 4];
    file.read_exact(&mut marker)
        .map_err(|_| "FLAC signature is truncated".to_owned())?;
    if &marker != b"fLaC" {
        return Err("FLAC signature is invalid".to_owned());
    }
    let mut metadata_bytes = 4u64;
    for _ in 0..MAX_FLAC_METADATA_BLOCKS {
        if cancel.is_some_and(|flag| flag.load(AtomicOrdering::Relaxed)) {
            return Err("FLAC safety scan was cancelled".to_owned());
        }
        let mut header = [0u8; 4];
        file.read_exact(&mut header)
            .map_err(|_| "FLAC metadata header is truncated".to_owned())?;
        let last = header[0] & 0x80 != 0;
        let block_type = header[0] & 0x7f;
        let length =
            (u32::from(header[1]) << 16) | (u32::from(header[2]) << 8) | u32::from(header[3]);
        metadata_bytes = metadata_bytes
            .checked_add(4 + u64::from(length))
            .ok_or_else(|| "FLAC metadata size overflow".to_owned())?;
        if metadata_bytes > MAX_FLAC_METADATA_BYTES {
            return Err(format!(
                "FLAC metadata exceeds the {MAX_FLAC_METADATA_BYTES} byte safety limit"
            ));
        }
        let content_start = file.stream_position().map_err(|error| error.to_string())?;
        let content_end = content_start
            .checked_add(u64::from(length))
            .ok_or_else(|| "FLAC metadata size overflow".to_owned())?;
        if content_end > file_len {
            return Err("FLAC metadata block extends beyond the file".to_owned());
        }
        if block_type == 4 {
            if length as usize > MAX_OGG_COMMENT_PACKET_BYTES {
                return Err(format!(
                    "FLAC comment block exceeds the {MAX_OGG_COMMENT_PACKET_BYTES} byte safety limit"
                ));
            }
            let mut comments = vec![0; length as usize];
            file.read_exact(&mut comments)
                .map_err(|_| "FLAC comment block is truncated".to_owned())?;
            validate_ogg_comment_packet(&comments, b"")?;
        } else {
            file.seek(std::io::SeekFrom::Start(content_end))
                .map_err(|error| error.to_string())?;
        }
        if last {
            return Ok(());
        }
    }
    Err(format!(
        "FLAC metadata exceeds the {MAX_FLAC_METADATA_BLOCKS} block safety limit"
    ))
}

fn skip_id3_prefixes(file: &mut File, cancel: Option<&AtomicBool>) -> Result<(bool, bool), String> {
    let file_len = file.metadata().map_err(|error| error.to_string())?.len();
    let mut total_id3_bytes = 0u64;
    for tag_index in 0..=MAX_ID3_TAGS {
        if cancel.is_some_and(|flag| flag.load(AtomicOrdering::Relaxed)) {
            return Err("media safety scan was cancelled".to_owned());
        }
        let start = file.stream_position().map_err(|error| error.to_string())?;
        let mut signature = [0u8; 3];
        let read = read_prefix(file, &mut signature).map_err(|error| error.to_string())?;
        if read < signature.len() || &signature != b"ID3" {
            file.seek(std::io::SeekFrom::Start(start))
                .map_err(|error| error.to_string())?;
            let mut container = [0u8; 4];
            let read = read_prefix(file, &mut container).map_err(|error| error.to_string())?;
            file.seek(std::io::SeekFrom::Start(start))
                .map_err(|error| error.to_string())?;
            return Ok((
                read == container.len() && &container == b"OggS",
                tag_index > 0,
            ));
        }
        if tag_index == MAX_ID3_TAGS {
            return Err(format!(
                "media contains more than {MAX_ID3_TAGS} leading ID3 tags"
            ));
        }
        let mut rest = [0u8; 7];
        file.read_exact(&mut rest)
            .map_err(|_| "ID3 header is truncated".to_owned())?;
        if rest[3..7].iter().any(|byte| byte & 0x80 != 0) {
            return Err("ID3 tag has an invalid synchsafe size".to_owned());
        }
        let size = (u32::from(rest[3]) << 21)
            | (u32::from(rest[4]) << 14)
            | (u32::from(rest[5]) << 7)
            | u32::from(rest[6]);
        if size > MAX_ID3_TAG_BYTES {
            return Err(format!(
                "ID3 tag exceeds the {MAX_ID3_TAG_BYTES} byte safety limit"
            ));
        }
        let footer = u64::from(rest[0] == 4 && rest[2] & 0x10 != 0) * 10;
        let tag_bytes = 10u64 + u64::from(size) + footer;
        total_id3_bytes = total_id3_bytes
            .checked_add(tag_bytes)
            .ok_or_else(|| "ID3 size overflow".to_owned())?;
        if total_id3_bytes > MAX_ID3_TOTAL_BYTES {
            return Err(format!(
                "leading ID3 tags exceed the {MAX_ID3_TOTAL_BYTES} byte safety limit"
            ));
        }
        let next = start
            .checked_add(tag_bytes)
            .ok_or_else(|| "ID3 size overflow".to_owned())?;
        if next > file_len {
            return Err("ID3 tag extends beyond the file".to_owned());
        }
        file.seek(std::io::SeekFrom::Start(next))
            .map_err(|error| error.to_string())?;
    }
    Ok((false, false))
}

fn read_prefix(file: &mut File, output: &mut [u8]) -> std::io::Result<usize> {
    let mut read = 0;
    while read < output.len() {
        match file.read(&mut output[read..]) {
            Ok(0) => break,
            Ok(count) => read += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(read)
}

fn validate_ogg_headers(
    file: &mut File,
    cancel: Option<&AtomicBool>,
    scan_audio_packets: bool,
) -> Result<(), String> {
    let mut serial = None;
    let mut seen_serials = HashSet::new();
    let mut expected_sequence = None;
    let mut packet_index = 0usize;
    let mut packet = Vec::new();
    let mut packet_len = 0usize;
    let mut codec = OggHeaderCodec::Unknown;
    loop {
        if cancel.is_some_and(|flag| flag.load(AtomicOrdering::Relaxed)) {
            return Err("Ogg safety scan was cancelled".to_owned());
        }
        let mut header = [0u8; 27];
        let mut read = 0;
        while read < header.len() {
            match file.read(&mut header[read..]) {
                Ok(0) if read == 0 => return Ok(()),
                Ok(0) => return Err("Ogg page header is truncated".to_owned()),
                Ok(count) => read += count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        if &header[..4] != b"OggS" || header[4] != 0 {
            return Err("Ogg page header is invalid".to_owned());
        }
        let page_serial = u32::from_le_bytes(header[14..18].try_into().unwrap());
        let sequence = u32::from_le_bytes(header[18..22].try_into().unwrap());
        if let Some(first_serial) = serial {
            if page_serial != first_serial {
                let complete = match codec {
                    OggHeaderCodec::Opus | OggHeaderCodec::Speex => packet_index >= 2,
                    OggHeaderCodec::Vorbis | OggHeaderCodec::Unknown => packet_index >= 3,
                };
                if !complete || seen_serials.contains(&page_serial) {
                    return Err(
                        "interleaved Ogg streams are not supported for tag reading".to_owned()
                    );
                }
                if seen_serials.len() >= 32 {
                    return Err("Ogg stream exceeds the 32 chained stream safety limit".to_owned());
                }
                serial = Some(page_serial);
                seen_serials.insert(page_serial);
                expected_sequence = None;
                packet_index = 0;
                packet.clear();
                packet_len = 0;
                codec = OggHeaderCodec::Unknown;
            }
        } else {
            serial = Some(page_serial);
            seen_serials.insert(page_serial);
        }
        if expected_sequence.is_some_and(|expected| expected != sequence) {
            return Err("Ogg header pages are out of sequence".to_owned());
        }
        expected_sequence = Some(sequence.wrapping_add(1));
        let continued = header[5] & 1 != 0;
        if continued != (packet_len != 0) {
            return Err("Ogg header packet continuation is invalid".to_owned());
        }
        let segment_count = usize::from(header[26]);
        let mut lacing = vec![0; segment_count];
        file.read_exact(&mut lacing)
            .map_err(|_| "Ogg segment table is truncated".to_owned())?;
        let payload_len: usize = lacing.iter().map(|&length| usize::from(length)).sum();
        let mut payload = vec![0; payload_len];
        file.read_exact(&mut payload)
            .map_err(|_| "Ogg page payload is truncated".to_owned())?;
        let mut offset = 0;
        for segment in lacing {
            let length = usize::from(segment);
            let packet_limit = match (packet_index, codec) {
                (0, _) => MAX_OGG_CODEC_HEADER_BYTES,
                (1, OggHeaderCodec::Vorbis | OggHeaderCodec::Opus | OggHeaderCodec::Speex) => {
                    MAX_OGG_COMMENT_PACKET_BYTES
                }
                _ => MAX_OGG_HEADER_PACKET_BYTES,
            };
            if packet_len.saturating_add(length) > packet_limit {
                return Err(format!(
                    "Ogg packet exceeds the {packet_limit} byte safety limit"
                ));
            }
            if packet_index < 3 {
                packet.extend_from_slice(&payload[offset..offset + length]);
            }
            packet_len += length;
            offset += length;
            if segment < 255 {
                match packet_index {
                    0 => {
                        codec = if packet.starts_with(b"\x01vorbis") {
                            OggHeaderCodec::Vorbis
                        } else if packet.starts_with(b"OpusHead") {
                            OggHeaderCodec::Opus
                        } else if packet.starts_with(b"Speex   ") {
                            OggHeaderCodec::Speex
                        } else {
                            OggHeaderCodec::Unknown
                        };
                    }
                    1 => match codec {
                        OggHeaderCodec::Vorbis => {
                            validate_ogg_comment_packet(&packet, b"\x03vorbis")?;
                        }
                        OggHeaderCodec::Opus => {
                            validate_ogg_comment_packet(&packet, b"OpusTags")?;
                        }
                        OggHeaderCodec::Speex => validate_ogg_comment_packet(&packet, b"")?,
                        _ => {}
                    },
                    _ => {}
                }
                packet.clear();
                packet_len = 0;
                packet_index += 1;
                let headers_complete = match codec {
                    OggHeaderCodec::Opus | OggHeaderCodec::Speex => packet_index >= 2,
                    OggHeaderCodec::Vorbis | OggHeaderCodec::Unknown => packet_index >= 3,
                };
                if headers_complete && !scan_audio_packets {
                    return Ok(());
                }
            }
        }
    }
}

pub(super) fn validate_ogg_audio_packet_limits(
    path: &Path,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let (is_ogg, _) = skip_id3_prefixes(&mut file, Some(cancel))?;
    if !is_ogg {
        return Ok(());
    }
    validate_ogg_headers(&mut file, Some(cancel), true)
}

pub(super) fn identity_container(
    path: &Path,
    cancel: &AtomicBool,
) -> Result<Option<&'static str>, String> {
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let (is_ogg, had_id3) = skip_id3_prefixes(&mut file, Some(cancel))?;
    if is_ogg {
        // Ogg is accepted only at byte zero; prefixed Ogg is too ambiguous for
        // the restricted packet identity probe.
        return Ok((!had_id3).then_some("ogg"));
    }
    let mut marker = [0u8; 4];
    if read_prefix(&mut file, &mut marker).map_err(|error| error.to_string())? != 4 {
        return Ok(None);
    }
    if &marker == b"fLaC" {
        return Ok(Some("flac"));
    }
    if is_mpeg_layer3_header(&marker) {
        return Ok(Some("mp3"));
    }
    Ok(None)
}

fn is_mpeg_layer3_header(marker: &[u8; 4]) -> bool {
    if !matches!(marker[1], 0xfa | 0xfb | 0xf2 | 0xf3 | 0xe2 | 0xe3) {
        return false;
    }
    let header = u32::from_be_bytes(*marker);
    (header >> 21) == 0x7ff
        && ((header >> 19) & 0x3) != 0x1
        && ((header >> 17) & 0x3) == 0x1
        && ((header >> 12) & 0xf) != 0xf
        && ((header >> 10) & 0x3) != 0x3
}

#[derive(Clone, Copy)]
enum OggHeaderCodec {
    Unknown,
    Vorbis,
    Opus,
    Speex,
}

fn validate_ogg_comment_packet(packet: &[u8], signature: &[u8]) -> Result<(), String> {
    if !packet.starts_with(signature) {
        return Err("Ogg comment packet signature is invalid".to_owned());
    }
    let mut offset = signature.len();
    let vendor_len = read_ogg_u32(packet, &mut offset)?;
    skip_ogg_bytes(packet, &mut offset, vendor_len)?;
    let count = read_ogg_u32(packet, &mut offset)? as usize;
    if count > MAX_OGG_COMMENT_ITEMS {
        return Err(format!(
            "Ogg comment packet exceeds the {MAX_OGG_COMMENT_ITEMS} item safety limit"
        ));
    }
    for _ in 0..count {
        let length = read_ogg_u32(packet, &mut offset)?;
        skip_ogg_bytes(packet, &mut offset, length)?;
    }
    Ok(())
}

fn read_ogg_u32(packet: &[u8], offset: &mut usize) -> Result<u32, String> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| "Ogg comment length overflow".to_owned())?;
    let bytes: [u8; 4] = packet
        .get(*offset..end)
        .ok_or_else(|| "Ogg comment packet is truncated".to_owned())?
        .try_into()
        .expect("slice length was checked");
    *offset = end;
    Ok(u32::from_le_bytes(bytes))
}

fn skip_ogg_bytes(packet: &[u8], offset: &mut usize, length: u32) -> Result<(), String> {
    let length = usize::try_from(length).map_err(|_| "Ogg comment length overflow".to_owned())?;
    let end = offset
        .checked_add(length)
        .ok_or_else(|| "Ogg comment length overflow".to_owned())?;
    if end > packet.len() {
        return Err("Ogg comment packet is truncated".to_owned());
    }
    *offset = end;
    Ok(())
}
