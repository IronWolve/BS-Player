//! Live internet-radio stream reader.
//!
//! Self-contained and panic-free: no UI, no audio engine, no profile access. One
//! network thread per stream feeds a bounded ring buffer; the audio decoder reads
//! from that buffer through [`LiveStream`] (`Read`, not seekable). Everything a
//! server controls is bounded: header size and count, redirects, playlist size,
//! metadata blocks, titles, the ring buffer, and every wait has a deadline or an
//! idle limit.
//!
//! The HTTP client is written here on purpose: Shoutcast v1 servers answer with
//! an `ICY 200 OK` status line that general HTTP libraries reject, and a live
//! stream needs an idle read limit and a socket that another thread can shut
//! down. HTTPS uses rustls with the bundled web root certificates (no system
//! libraries, no async runtime). Plain http and https share one code path above
//! the transport; the https transport itself is covered by a loopback test with a
//! throwaway certificate.
//!
//! Supported audio: MP3, AAC (ADTS) and Ogg Vorbis (and FLAC) streams. HLS and Opus
//! are recognised and refused with a clear message. Playlists (`.pls`, `.m3u`)
//! are followed to their first http(s) entry.

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

// ---------------------------------------------------------------------------
// Limits and timings
// ---------------------------------------------------------------------------

/// Sent as `User-Agent`; carries no version or personal information.
const USER_AGENT: &str = "BS-Player";
/// Redirects followed for one address.
const MAX_REDIRECTS: u32 = 5;
/// Playlist documents followed on the way to a stream.
const MAX_PLAYLIST_LEVELS: u32 = 3;
/// Longest accepted address.
const MAX_URL_BYTES: usize = 2048;
/// Total size of a response head (status line and headers).
const MAX_HEAD_BYTES: usize = 16 * 1024;
/// Longest single status or header line.
const MAX_LINE_BYTES: usize = 8 * 1024;
/// Most header lines accepted.
const MAX_HEADERS: usize = 100;
/// Interim (1xx) responses skipped before the real one.
const MAX_INTERIM: usize = 3;
/// Playlist bytes read; the rest is ignored.
const MAX_PLAYLIST_BYTES: usize = 64 * 1024;
/// Playlist entries considered.
const MAX_PLAYLIST_ENTRIES: usize = 64;
/// Ring buffer limits (bytes) and the amount of audio it aims to hold.
const MIN_RING_BYTES: usize = 256 * 1024;
const MAX_RING_BYTES: usize = 4 * 1024 * 1024;
const RING_SECONDS: u64 = 8;
/// Bitrate assumed when the station does not report one.
const DEFAULT_KBPS: u32 = 128;
/// Bytes read from the network per call.
const NET_CHUNK: usize = 8 * 1024;
/// Audio bytes needed before the format check and before `open_stream` returns.
const MIN_PROBE_BYTES: usize = 1024;
/// Bytes of the buffered audio looked at when checking the format.
const SNIFF_WINDOW: usize = 8 * 1024;
/// Bytes looked at to tell a playlist or web page from audio.
const PEEK_BYTES: usize = 64;
/// After the first bytes of a response, how long to wait for more before judging it.
const PEEK_GRACE: Duration = Duration::from_millis(300);
/// Largest ICY metadata block (255 * 16, per the protocol).
const MAX_METADATA_BYTES: usize = 4080;
/// Smallest metadata interval accepted; anything lower is a broken server.
const MIN_METAINT: u64 = 64;
/// Longest title and station text kept (characters).
const MAX_TITLE_CHARS: usize = 256;
const MAX_NAME_CHARS: usize = 128;
/// Titles waiting for the listener to reach their position.
const MAX_PENDING_TITLES: usize = 16;
/// How often blocked threads re-check the cancel flag.
const POLL: Duration = Duration::from_millis(100);
/// Socket read slice; `Patient` turns these into one idle limit.
const SOCKET_SLICE: Duration = Duration::from_millis(250);
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// A connection must have run this long and delivered data before back-off resets.
const GOOD_SESSION: Duration = Duration::from_secs(10);
/// Addresses tried per host name.
const MAX_ADDRS: usize = 4;

const MSG_HLS: &str = "HLS streams are not supported";
const MSG_OPUS: &str = "Opus streams are not supported yet";
const MSG_WEB_PAGE: &str = "This address is a web page, not an audio stream";

/// Every wait in the module comes from here so tests can run fast.
#[derive(Clone, Copy, Debug)]
struct Timing {
    /// Time allowed to open one TCP connection (all addresses together).
    connect: Duration,
    /// No data for this long ends a connection (also the header and body idle limit).
    read: Duration,
    /// Total time `open_stream` may take.
    open: Duration,
    /// Reconnect delays; the last one repeats.
    backoff_ms: [u64; 4],
}

impl Timing {
    const PRODUCTION: Timing = Timing {
        connect: Duration::from_secs(10),
        read: Duration::from_secs(15),
        open: Duration::from_secs(15),
        backoff_ms: Backoff::DEFAULT_SCHEDULE_MS,
    };
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// What the server told us about the station.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StreamInfo {
    pub name: String,
    pub genre: String,
    pub bitrate_kbps: Option<u32>,
    /// Lower-case media type without parameters, e.g. `audio/mpeg`.
    pub content_type: String,
    /// Final address after redirects (and after any playlist).
    pub url: String,
}

/// Why a stream could not be opened. `Display` is a short sentence for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamError {
    Unsupported(String),
    Network(String),
    Http(u16),
    Cancelled,
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StreamError::Unsupported(message) | StreamError::Network(message) => {
                f.write_str(message)
            }
            StreamError::Http(code) => {
                let hint = match code {
                    401 | 403 => " (access denied)",
                    404 | 410 => " (not found)",
                    500..=599 => " (server problem)",
                    _ => "",
                };
                write!(f, "The server returned error {code}{hint}")
            }
            StreamError::Cancelled => f.write_str("Cancelled"),
        }
    }
}

impl std::error::Error for StreamError {}

fn net(message: impl Into<String>) -> StreamError {
    StreamError::Network(message.into())
}

fn unsupported(message: impl Into<String>) -> StreamError {
    StreamError::Unsupported(message.into())
}

/// Connection state shown to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamStatus {
    Connecting,
    Playing,
    Reconnecting { attempt: u32 },
    Failed(String),
}

// ---------------------------------------------------------------------------
// Back-off
// ---------------------------------------------------------------------------

/// Reconnect schedule: 1 s, 2 s, 5 s, then every 15 s.
#[derive(Debug, Clone)]
pub struct Backoff {
    attempt: u32,
    schedule_ms: [u64; 4],
}

impl Backoff {
    pub const DEFAULT_SCHEDULE_MS: [u64; 4] = [1_000, 2_000, 5_000, 15_000];

    pub fn new() -> Self {
        Self::with_schedule_ms(Self::DEFAULT_SCHEDULE_MS)
    }

    /// The last entry repeats forever.
    pub fn with_schedule_ms(schedule_ms: [u64; 4]) -> Self {
        Backoff {
            attempt: 0,
            schedule_ms,
        }
    }

    /// Counts a failed attempt and returns how long to wait before the next one.
    pub fn next_delay(&mut self) -> Duration {
        self.attempt = self.attempt.saturating_add(1);
        let index = usize::try_from(self.attempt - 1)
            .unwrap_or(usize::MAX)
            .min(3);
        Duration::from_millis(self.schedule_ms.get(index).copied().unwrap_or(15_000))
    }

    /// Failed attempts since the last reset (1 on the first retry).
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    /// Resets only after a connection that ran long enough and delivered data, so
    /// a server that accepts and immediately hangs up keeps backing off.
    pub fn note_session(&mut self, lasted: Duration, bytes: u64) {
        if bytes > 0 && lasted >= GOOD_SESSION {
            self.reset();
        }
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Addresses
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct Url {
    https: bool,
    /// Lower-case host name or IP literal (without brackets).
    host: String,
    port: u16,
    /// Path and query, always starting with `/`, with unsafe bytes percent-encoded.
    target: String,
}

impl Url {
    fn parse(input: &str) -> Result<Url, StreamError> {
        let text = input.trim();
        if text.is_empty() {
            return Err(unsupported(
                "Enter a stream address that starts with http:// or https://",
            ));
        }
        if text.len() > MAX_URL_BYTES {
            return Err(unsupported("The address is too long"));
        }
        if text.chars().any(char::is_control) {
            return Err(unsupported("The address contains invalid characters"));
        }
        let Some((scheme, rest)) = text.split_once("://") else {
            return Err(unsupported(
                "The address must start with http:// or https://",
            ));
        };
        let https = if scheme.eq_ignore_ascii_case("https") {
            true
        } else if scheme.eq_ignore_ascii_case("http") {
            false
        } else {
            return Err(unsupported(
                "Only http:// and https:// stream addresses are supported",
            ));
        };
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(end);
        if authority.contains('@') {
            return Err(unsupported(
                "Addresses with a user name or password are not supported",
            ));
        }
        let bad_host = || unsupported("The address has an invalid host name");
        let (host, port_text) = if let Some(inner) = authority.strip_prefix('[') {
            let close = inner.find(']').ok_or_else(bad_host)?;
            let (host, after) = inner.split_at(close);
            let after = after.get(1..).unwrap_or("");
            let port_text = if after.is_empty() {
                None
            } else {
                Some(after.strip_prefix(':').ok_or_else(bad_host)?)
            };
            if host.parse::<std::net::Ipv6Addr>().is_err() {
                return Err(bad_host());
            }
            (host, port_text)
        } else {
            let (host, port_text) = match authority.rsplit_once(':') {
                Some((host, port_text)) => (host, Some(port_text)),
                None => (authority, None),
            };
            let valid = !host.is_empty()
                && host.len() <= 253
                && host
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'));
            if !valid {
                return Err(bad_host());
            }
            (host, port_text)
        };
        let port = match port_text {
            None => {
                if https {
                    443
                } else {
                    80
                }
            }
            Some(digits) => digits
                .parse::<u16>()
                .ok()
                .filter(|port| *port != 0)
                .ok_or_else(|| unsupported("The address has an invalid port number"))?,
        };
        let tail = tail.split('#').next().unwrap_or("");
        let raw_target = if tail.is_empty() {
            "/".to_string()
        } else if tail.starts_with('?') {
            format!("/{tail}")
        } else {
            tail.to_string()
        };
        Ok(Url {
            https,
            host: host.to_ascii_lowercase(),
            port,
            target: encode_target(&raw_target),
        })
    }

    fn scheme(&self) -> &'static str {
        if self.https { "https" } else { "http" }
    }

    fn default_port(&self) -> u16 {
        if self.https { 443 } else { 80 }
    }

    /// `host` or `host:port` (brackets around IPv6), as used in `Host:` and URLs.
    fn authority(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        if self.port == self.default_port() {
            host
        } else {
            format!("{host}:{}", self.port)
        }
    }

    fn to_url_string(&self) -> String {
        format!("{}://{}{}", self.scheme(), self.authority(), self.target)
    }

    /// Resolves a redirect `Location` (absolute, scheme-relative, absolute-path,
    /// query-only or relative) against this address.
    fn join(&self, location: &str) -> Result<Url, StreamError> {
        let loc = location.trim();
        if loc.is_empty() {
            return Err(net("The server redirected without saying where"));
        }
        if let Some(index) = loc.find("://") {
            let scheme = loc.get(..index).unwrap_or("");
            if !scheme.is_empty()
                && scheme
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
            {
                return Url::parse(loc);
            }
        }
        if let Some(rest) = loc.strip_prefix("//") {
            return Url::parse(&format!("{}://{rest}", self.scheme()));
        }
        let path = self.target.split('?').next().unwrap_or("/");
        let target = if loc.starts_with('/') {
            loc.to_string()
        } else if loc.starts_with('?') {
            format!("{path}{loc}")
        } else {
            let dir = match path.rfind('/') {
                Some(index) => path.get(..=index).unwrap_or("/"),
                None => "/",
            };
            format!("{dir}{loc}")
        };
        Url::parse(&format!("{}://{}{target}", self.scheme(), self.authority()))
    }

    /// True when the path looks like a playlist document.
    #[allow(dead_code)] // only `resolve_playlist_url` (not used by the app) needs it
    fn looks_like_playlist(&self) -> bool {
        let path = self
            .target
            .split(['?', '#'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        path.ends_with(".pls") || path.ends_with(".m3u") || path.ends_with(".m3u8")
    }
}

/// Percent-encodes the bytes that must not appear raw in a request target.
fn encode_target(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        let unsafe_byte = byte <= 0x20
            || byte >= 0x7F
            || matches!(
                byte,
                b'"' | b'<' | b'>' | b'\\' | b'^' | b'`' | b'{' | b'|' | b'}'
            );
        if unsafe_byte {
            out.push_str(&format!("%{byte:02X}"));
        } else {
            out.push(char::from(byte));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Text helpers
// ---------------------------------------------------------------------------

/// Windows-1252 byte to character (the five undefined bytes become U+FFFD).
fn cp1252_char(byte: u8) -> char {
    const HIGH: [char; 32] = [
        '\u{20AC}', '\u{FFFD}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}',
        '\u{2021}', '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{FFFD}',
        '\u{017D}', '\u{FFFD}', '\u{FFFD}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}',
        '\u{2022}', '\u{2013}', '\u{2014}', '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}',
        '\u{0153}', '\u{FFFD}', '\u{017E}', '\u{0178}',
    ];
    if (0x80..0xA0).contains(&byte) {
        HIGH.get(usize::from(byte - 0x80))
            .copied()
            .unwrap_or('\u{FFFD}')
    } else {
        char::from(byte)
    }
}

/// UTF-8 when valid, otherwise Windows-1252 (what old servers send).
fn decode_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => bytes.iter().map(|byte| cp1252_char(*byte)).collect(),
    }
}

/// Removes control and direction-override characters, collapses white space, trims
/// and limits the length (in characters).
fn clean_text(text: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(text.len().min(max_chars.saturating_mul(4)));
    let mut count = 0usize;
    let mut pending_space = false;
    for ch in text.chars() {
        let invisible = matches!(
            ch,
            '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{2069}'
                | '\u{FEFF}'
        );
        if invisible {
            continue;
        }
        if ch.is_control() || ch.is_whitespace() {
            pending_space = true;
            continue;
        }
        if count >= max_chars {
            break;
        }
        if pending_space && !out.is_empty() {
            if count + 1 >= max_chars {
                break;
            }
            out.push(' ');
            count += 1;
        }
        pending_space = false;
        out.push(ch);
        count += 1;
    }
    out
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(start, |i| i + 1);
    bytes.get(start..end.max(start)).unwrap_or(&[])
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

// ---------------------------------------------------------------------------
// ICY metadata
// ---------------------------------------------------------------------------

/// Extracts the `StreamTitle` value from one metadata block. `Some("")` means the
/// server explicitly sent an empty title; `None` means the block has no title.
fn parse_icy_metadata(block: &[u8]) -> Option<String> {
    let end = block
        .iter()
        .rposition(|b| *b != 0)
        .map_or(0, |index| index + 1);
    let block = block.get(..end)?;
    const KEY: &[u8] = b"StreamTitle=";
    let start = find_subslice(block, KEY)?;
    let rest = block.get(start + KEY.len()..)?;
    let quote = *rest.first()?;
    if quote != b'\'' && quote != b'"' {
        let stop = rest.iter().position(|b| *b == b';').unwrap_or(rest.len());
        return Some(clean_text(&decode_text(rest.get(..stop)?), MAX_TITLE_CHARS));
    }
    let body = rest.get(1..)?;
    // The value ends at the first quote followed by `;` and then either the end of
    // the block or another `Key=`; quotes inside the title ("Don't", "Rock';n'Roll")
    // therefore stay part of it.
    let mut value_end = None;
    for (index, byte) in body.iter().enumerate() {
        if *byte != quote || body.get(index + 1) != Some(&b';') {
            continue;
        }
        let after = body.get(index + 2..).unwrap_or(&[]);
        if after.is_empty() || starts_with_key(after) {
            value_end = Some(index);
            break;
        }
    }
    let value = match value_end {
        Some(index) => body.get(..index)?,
        None => match body.last() {
            Some(last) if *last == quote => body.get(..body.len() - 1)?,
            _ => body,
        },
    };
    Some(clean_text(&decode_text(value), MAX_TITLE_CHARS))
}

/// True when `bytes` starts with `Identifier=` (the next ICY key).
fn starts_with_key(bytes: &[u8]) -> bool {
    let mut seen = 0usize;
    for byte in bytes {
        if byte.is_ascii_alphanumeric() || *byte == b'_' {
            seen += 1;
        } else {
            return *byte == b'=' && seen > 0;
        }
    }
    false
}

enum DemuxState {
    Audio,
    Length,
    Metadata { want: usize, have: Vec<u8> },
}

/// Splits an ICY byte stream into audio and metadata, whatever the chunking.
struct IcyDemux {
    /// Audio bytes between metadata blocks; 0 means the stream has none.
    metaint: u64,
    until_meta: u64,
    state: DemuxState,
}

impl IcyDemux {
    fn new(metaint: u64) -> Self {
        IcyDemux {
            metaint,
            until_meta: metaint,
            state: DemuxState::Audio,
        }
    }

    /// Appends the audio of `input` to `audio`; every title found is appended to
    /// `titles` with its byte offset inside `audio`.
    fn feed(&mut self, mut input: &[u8], audio: &mut Vec<u8>, titles: &mut Vec<(usize, String)>) {
        if self.metaint == 0 {
            audio.extend_from_slice(input);
            return;
        }
        while !input.is_empty() {
            match std::mem::replace(&mut self.state, DemuxState::Audio) {
                DemuxState::Audio => {
                    let take = usize::try_from(self.until_meta)
                        .unwrap_or(usize::MAX)
                        .min(input.len());
                    let (head, rest) = input.split_at(take);
                    audio.extend_from_slice(head);
                    input = rest;
                    self.until_meta = self.until_meta.saturating_sub(take as u64);
                    self.state = if self.until_meta == 0 {
                        DemuxState::Length
                    } else {
                        DemuxState::Audio
                    };
                }
                DemuxState::Length => {
                    let length = usize::from(input.first().copied().unwrap_or(0)) * 16;
                    input = input.get(1..).unwrap_or(&[]);
                    if length == 0 {
                        self.until_meta = self.metaint;
                        self.state = DemuxState::Audio;
                    } else {
                        self.state = DemuxState::Metadata {
                            want: length.min(MAX_METADATA_BYTES),
                            have: Vec::with_capacity(length.min(MAX_METADATA_BYTES)),
                        };
                    }
                }
                DemuxState::Metadata { want, mut have } => {
                    let take = want.saturating_sub(have.len()).min(input.len());
                    let (head, rest) = input.split_at(take);
                    have.extend_from_slice(head);
                    input = rest;
                    if have.len() >= want {
                        if let Some(title) = parse_icy_metadata(&have) {
                            titles.push((audio.len(), title));
                        }
                        self.until_meta = self.metaint;
                        self.state = DemuxState::Audio;
                    } else {
                        self.state = DemuxState::Metadata { want, have };
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Content classification and format sniffing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A media type that carries a supported audio stream.
    Audio,
    /// A playlist (`.pls`, `.m3u`, possibly HLS).
    Playlist,
    Opus,
    /// A web page.
    Web,
    /// Missing, generic or unknown: decide from the first bytes.
    Unknown,
}

fn classify_content_type(content_type: &str) -> Kind {
    match content_type {
        "audio/mpeg" | "audio/mp3" | "audio/mpeg3" | "audio/x-mpeg" | "audio/x-mp3"
        | "audio/mpg" | "audio/aac" | "audio/aacp" | "audio/x-aac" | "audio/x-aacp"
        | "audio/aac+" | "audio/mp4a-latm" | "audio/ogg" | "application/ogg" | "audio/x-ogg"
        | "audio/vorbis" | "audio/x-vorbis" | "audio/flac" | "audio/x-flac" => Kind::Audio,
        "audio/x-mpegurl"
        | "audio/mpegurl"
        | "application/x-mpegurl"
        | "application/vnd.apple.mpegurl"
        | "application/vnd.apple.mpegurl.audio"
        | "audio/x-scpls"
        | "audio/scpls"
        | "application/x-scpls"
        | "application/pls+xml" => Kind::Playlist,
        "audio/opus" | "audio/x-opus" | "application/opus" => Kind::Opus,
        "text/html" | "application/xhtml+xml" => Kind::Web,
        _ => Kind::Unknown,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextKind {
    Pls,
    M3u,
    Hls,
    Html,
    Xml,
}

/// Recognises text documents by their first bytes. `allow_bare` also accepts a
/// list that starts directly with an http(s) address.
fn sniff_text(data: &[u8], allow_bare: bool) -> Option<TextKind> {
    let data = data.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(data);
    let start = data
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(data.len());
    let head: Vec<u8> = data
        .get(start..)
        .unwrap_or(&[])
        .iter()
        .take(64)
        .map(u8::to_ascii_lowercase)
        .collect();
    let starts = |prefix: &[u8]| head.starts_with(prefix);
    if starts(b"[playlist]") {
        Some(TextKind::Pls)
    } else if starts(b"#extm3u") {
        Some(TextKind::M3u)
    } else if starts(b"#ext-x-") {
        Some(TextKind::Hls)
    } else if starts(b"<!doctype html")
        || starts(b"<html")
        || starts(b"<head")
        || starts(b"<body")
        || starts(b"<title")
    {
        Some(TextKind::Html)
    } else if starts(b"<?xml") || starts(b"<asx") || starts(b"<playlist") || starts(b"<rss") {
        Some(TextKind::Xml)
    } else if allow_bare && (starts(b"http://") || starts(b"https://")) {
        Some(TextKind::M3u)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sniff {
    Mp3,
    Adts,
    OggVorbis,
    OggOpus,
    OggOther,
    Flac,
    Unknown,
}

const BITRATES_V1_L1: [u32; 14] = [
    32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448,
];
const BITRATES_V1_L2: [u32; 14] = [
    32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
];
const BITRATES_V1_L3: [u32; 14] = [
    32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
];
const BITRATES_V2_L1: [u32; 14] = [
    32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256,
];
const BITRATES_V2_L23: [u32; 14] = [8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];

/// Frame length and (version, layer, sample-rate index) of an MPEG audio header.
fn mp3_header(bytes: &[u8]) -> Option<(usize, u8, u8, u8)> {
    let b0 = *bytes.first()?;
    let b1 = *bytes.get(1)?;
    let b2 = *bytes.get(2)?;
    if b0 != 0xFF || (b1 & 0xE0) != 0xE0 {
        return None;
    }
    let version = (b1 >> 3) & 3;
    let layer = (b1 >> 1) & 3;
    if version == 1 || layer == 0 {
        return None;
    }
    let bitrate_index = usize::from(b2 >> 4);
    let rate_index = usize::from((b2 >> 2) & 3);
    if bitrate_index == 0 || bitrate_index == 15 || rate_index == 3 {
        return None;
    }
    let mpeg1 = version == 3;
    let table = match (mpeg1, layer) {
        (true, 3) => &BITRATES_V1_L1,
        (true, 2) => &BITRATES_V1_L2,
        (true, _) => &BITRATES_V1_L3,
        (false, 3) => &BITRATES_V2_L1,
        (false, _) => &BITRATES_V2_L23,
    };
    let bitrate = table.get(bitrate_index - 1).copied()? * 1000;
    let rates: [u32; 3] = match version {
        3 => [44_100, 48_000, 32_000],
        2 => [22_050, 24_000, 16_000],
        _ => [11_025, 12_000, 8_000],
    };
    let rate = rates.get(rate_index).copied()?;
    let padding = u32::from((b2 >> 1) & 1);
    let length = match layer {
        3 => (12 * bitrate / rate + padding) * 4,
        2 => 144 * bitrate / rate + padding,
        _ if mpeg1 => 144 * bitrate / rate + padding,
        _ => 72 * bitrate / rate + padding,
    };
    Some((
        usize::try_from(length).ok()?,
        version,
        layer,
        rate_index as u8,
    ))
}

/// Frame length and sampling-frequency index of an ADTS header.
fn adts_header(bytes: &[u8]) -> Option<(usize, u8)> {
    let b: [u8; 6] = bytes.get(..6)?.try_into().ok()?;
    if b[0] != 0xFF || (b[1] & 0xF6) != 0xF0 {
        return None;
    }
    let rate_index = (b[2] >> 2) & 0x0F;
    if rate_index >= 13 {
        return None;
    }
    let length = (usize::from(b[3] & 3) << 11) | (usize::from(b[4]) << 3) | usize::from(b[5] >> 5);
    if length < 7 {
        return None;
    }
    Some((length, rate_index))
}

/// True when two consecutive MPEG audio frames are found in `data`.
fn looks_like_mp3(data: &[u8]) -> bool {
    let limit = data.len().min(SNIFF_WINDOW);
    for index in 0..limit {
        let Some((length, version, layer, rate)) = data.get(index..).and_then(mp3_header) else {
            continue;
        };
        if let Some((_, v2, l2, r2)) = data.get(index + length..).and_then(mp3_header)
            && (v2, l2, r2) == (version, layer, rate)
        {
            return true;
        }
    }
    false
}

/// True when two consecutive ADTS frames are found in `data`.
fn looks_like_adts(data: &[u8]) -> bool {
    let limit = data.len().min(SNIFF_WINDOW);
    for index in 0..limit {
        let Some((length, rate)) = data.get(index..).and_then(adts_header) else {
            continue;
        };
        if let Some((_, r2)) = data.get(index + length..).and_then(adts_header)
            && r2 == rate
        {
            return true;
        }
    }
    false
}

fn sniff_audio(data: &[u8]) -> Sniff {
    if data.starts_with(b"OggS") {
        let segments = usize::from(data.get(26).copied().unwrap_or(0));
        let payload = data.get(27 + segments..).unwrap_or(&[]);
        return if payload.starts_with(b"OpusHead") {
            Sniff::OggOpus
        } else if payload.starts_with(b"\x01vorbis") {
            Sniff::OggVorbis
        } else {
            Sniff::OggOther
        };
    }
    if data.starts_with(b"fLaC") {
        Sniff::Flac
    } else if data.starts_with(b"ID3") || looks_like_mp3(data) {
        Sniff::Mp3
    } else if looks_like_adts(data) {
        Sniff::Adts
    } else {
        Sniff::Unknown
    }
}

/// Decides whether the first buffered bytes of a stream are acceptable.
/// `trusted` is true when the server's media type already named an audio format.
fn check_audio_prefix(data: &[u8], trusted: bool) -> Result<(), StreamError> {
    match sniff_audio(data) {
        Sniff::OggOpus => Err(unsupported(MSG_OPUS)),
        Sniff::OggOther => Err(unsupported("This Ogg stream type is not supported")),
        Sniff::Mp3 | Sniff::Adts | Sniff::OggVorbis | Sniff::Flac => Ok(()),
        Sniff::Unknown if trusted => Ok(()),
        Sniff::Unknown => Err(unsupported(
            "This address does not look like a supported audio stream",
        )),
    }
}

// ---------------------------------------------------------------------------
// Playlists
// ---------------------------------------------------------------------------

/// Candidate stream addresses of a `.pls` or `.m3u` document, in order. HLS
/// documents are refused.
fn parse_playlist(bytes: &[u8]) -> Result<Vec<String>, StreamError> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let text = decode_text(bytes).replace('\r', "\n");
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    if lines.iter().any(|line| line.starts_with("#EXT-X-")) {
        return Err(unsupported(MSG_HLS));
    }
    let is_pls = lines
        .iter()
        .any(|line| line.eq_ignore_ascii_case("[playlist]"));
    let mut entries: Vec<(u32, &str)> = Vec::new();
    if is_pls {
        for line in &lines {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            if let Some(number) = key.strip_prefix("file")
                && let Ok(number) = number.parse::<u32>()
            {
                entries.push((number, value.trim()));
            }
        }
        entries.sort_by_key(|entry| entry.0);
    } else {
        for line in &lines {
            if !line.is_empty() && !line.starts_with('#') {
                entries.push((0, *line));
            }
        }
    }
    let mut found = Vec::new();
    let mut other_scheme = false;
    for (_, entry) in entries {
        let lower = entry.get(..8).unwrap_or(entry).to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            if found.len() < MAX_PLAYLIST_ENTRIES {
                found.push(entry.to_string());
            }
        } else {
            other_scheme = true;
        }
    }
    if found.is_empty() {
        return Err(unsupported(if other_scheme {
            "This playlist uses an address type that is not supported"
        } else {
            "The playlist has no stream address"
        }));
    }
    Ok(found)
}

/// The first playlist entry that is a valid address.
fn first_valid_url(entries: &[String]) -> Result<Url, StreamError> {
    let mut first_error = None;
    for entry in entries {
        match Url::parse(entry) {
            Ok(url) => return Ok(url),
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    Err(first_error.unwrap_or_else(|| unsupported("The playlist has no stream address")))
}

// ---------------------------------------------------------------------------
// HTTP head, body framing and the patient transport
// ---------------------------------------------------------------------------

/// Marker error produced when the cancel flag stops a blocked read.
#[derive(Debug)]
struct CancelMarker;

impl std::fmt::Display for CancelMarker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for CancelMarker {}

fn is_cancel(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|inner| inner.downcast_ref::<CancelMarker>().is_some())
}

/// Cancel flag and idle limit shared by every read of one open/stream attempt.
struct Ctl {
    cancel: Arc<AtomicBool>,
    idle_ms: AtomicU64,
}

impl Ctl {
    fn new(cancel: Arc<AtomicBool>, idle: Duration) -> Arc<Ctl> {
        let ctl = Arc::new(Ctl {
            cancel,
            idle_ms: AtomicU64::new(1),
        });
        ctl.set_idle(idle);
        ctl
    }

    fn set_idle(&self, idle: Duration) {
        let millis = u64::try_from(idle.as_millis()).unwrap_or(u64::MAX).max(1);
        self.idle_ms.store(millis, Ordering::SeqCst);
    }

    /// Idle limit for the open phase: the usual limit, but never past `deadline`.
    fn set_idle_until(&self, idle: Duration, deadline: Instant) {
        let left = deadline.saturating_duration_since(Instant::now());
        self.set_idle(idle.min(left).max(Duration::from_millis(50)));
    }

    fn idle(&self) -> Duration {
        Duration::from_millis(self.idle_ms.load(Ordering::SeqCst))
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
}

/// Wraps a socket that has a short read timeout and turns it into a reader that
/// retries until data arrives, the idle limit passes, or the cancel flag is set.
struct Patient<T> {
    inner: T,
    ctl: Arc<Ctl>,
}

impl<T: Read> Read for Patient<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let started = Instant::now();
        loop {
            if self.ctl.cancelled() {
                return Err(io::Error::other(CancelMarker));
            }
            match self.inner.read(buf) {
                Ok(count) => return Ok(count),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) =>
                {
                    if started.elapsed() >= self.ctl.idle() {
                        return Err(io::Error::from(io::ErrorKind::TimedOut));
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }
}

impl<T: Write> Write for Patient<T> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.ctl.cancelled() {
            return Err(io::Error::other(CancelMarker));
        }
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Anything a response can be read from.
trait Transport: Read + Write + Send {}
impl<T: Read + Write + Send> Transport for T {}

/// Maps a transport error to a short user-facing error that names only the host.
fn io_to_error(host: &str, error: &io::Error) -> StreamError {
    if is_cancel(error) {
        return StreamError::Cancelled;
    }
    if let Some(tls) = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>())
    {
        return net(match tls {
            rustls::Error::InvalidCertificate(_) => format!(
                "The secure connection to {host} was refused: its certificate is not trusted"
            ),
            _ => format!("The secure connection to {host} failed"),
        });
    }
    net(match error.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => {
            format!("{host} did not answer in time")
        }
        io::ErrorKind::UnexpectedEof
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::BrokenPipe => format!("The connection to {host} was closed"),
        _ => format!("Could not read from {host}"),
    })
}

/// Reads one line of at most `max` bytes (line ending removed). `Ok(None)` is a
/// clean end of data before the first byte.
fn read_line_limited<R: BufRead>(
    reader: &mut R,
    max: usize,
    deadline: Option<Instant>,
) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        if let Some(deadline) = deadline
            && Instant::now() >= deadline
        {
            return Err(io::Error::from(io::ErrorKind::TimedOut));
        }
        let (finished, used) = {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                return if line.is_empty() {
                    Ok(None)
                } else {
                    Err(io::Error::from(io::ErrorKind::UnexpectedEof))
                };
            }
            let newline = available.iter().position(|b| *b == b'\n');
            let take = newline.unwrap_or(available.len());
            if line.len().saturating_add(take) > max {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
            }
            line.extend_from_slice(available.get(..take).unwrap_or(&[]));
            (newline.is_some(), take + usize::from(newline.is_some()))
        };
        reader.consume(used);
        if finished {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Some(line));
        }
    }
}

#[derive(Debug)]
struct Head {
    status: u16,
    /// Lower-case names; values decoded with `decode_text`.
    headers: Vec<(String, String)>,
}

impl Head {
    /// First header with this (lower-case) name.
    fn get(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// `HTTP/1.x NNN ...` or the Shoutcast `ICY NNN ...` status line.
fn parse_status_line(line: &[u8]) -> Option<u16> {
    let text = std::str::from_utf8(line).ok()?;
    let mut parts = text.splitn(3, ' ');
    let protocol = parts.next()?;
    if !(protocol.starts_with("HTTP/1.") || protocol == "ICY") {
        return None;
    }
    let code = parts.next()?.trim().parse::<u16>().ok()?;
    (100..=599).contains(&code).then_some(code)
}

fn read_head<R: BufRead>(reader: &mut R, deadline: Instant) -> Result<Head, StreamError> {
    let mut interim = 0usize;
    loop {
        let mut total = 0usize;
        let status_line = match read_line_limited(reader, MAX_LINE_BYTES, Some(deadline)) {
            Ok(Some(line)) => line,
            Ok(None) => return Err(net("The server closed the connection without answering")),
            Err(error) => return Err(head_error(&error)),
        };
        total += status_line.len();
        let status = parse_status_line(&status_line)
            .ok_or_else(|| net("The server did not send a valid web response"))?;
        let mut headers = Vec::new();
        let mut count = 0usize;
        loop {
            let line = match read_line_limited(reader, MAX_LINE_BYTES, Some(deadline)) {
                Ok(Some(line)) => line,
                Ok(None) => return Err(net("The server closed the connection early")),
                Err(error) => return Err(head_error(&error)),
            };
            total += line.len() + 2;
            if total > MAX_HEAD_BYTES {
                return Err(net("The server's response headers are too large"));
            }
            if line.is_empty() {
                break;
            }
            count += 1;
            if count > MAX_HEADERS {
                return Err(net("The server sent too many response headers"));
            }
            let Some(colon) = line.iter().position(|b| *b == b':') else {
                continue;
            };
            let name = String::from_utf8_lossy(line.get(..colon).unwrap_or(&[]))
                .trim()
                .to_ascii_lowercase();
            let valid_name = !name.is_empty()
                && name.len() <= 64
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
            if valid_name {
                let value = decode_text(trim_ascii(line.get(colon + 1..).unwrap_or(&[])));
                headers.push((name, value));
            }
        }
        if (100..200).contains(&status) && status != 101 && interim < MAX_INTERIM {
            interim += 1;
            continue;
        }
        return Ok(Head { status, headers });
    }
}

fn head_error(error: &io::Error) -> StreamError {
    if is_cancel(error) {
        return StreamError::Cancelled;
    }
    match error.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => {
            net("The station did not answer in time")
        }
        io::ErrorKind::InvalidData => net("The server's response headers are too large"),
        _ => net("The connection was lost before the station answered"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChunkState {
    Size,
    Data(u64),
    DataEnd,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Framing {
    UntilClose,
    Length(u64),
    Chunked(ChunkState),
    Empty,
}

fn framing_for(head: &Head) -> Result<Framing, StreamError> {
    if let Some(encoding) = head.get("content-encoding") {
        let encoding = encoding.trim().to_ascii_lowercase();
        if !encoding.is_empty() && encoding != "identity" {
            return Err(unsupported(
                "The server sent compressed data, which is not supported",
            ));
        }
    }
    if matches!(head.status, 204 | 304) || (100..200).contains(&head.status) {
        return Ok(Framing::Empty);
    }
    if let Some(encoding) = head.get("transfer-encoding") {
        return Ok(if encoding.to_ascii_lowercase().contains("chunked") {
            Framing::Chunked(ChunkState::Size)
        } else {
            Framing::UntilClose
        });
    }
    Ok(
        match head
            .get("content-length")
            .and_then(|value| value.trim().parse::<u64>().ok())
        {
            Some(length) => Framing::Length(length),
            None => Framing::UntilClose,
        },
    )
}

/// A response body: de-chunked, length-limited, with a small push-back buffer so
/// the first bytes can be inspected without being lost.
struct Body {
    reader: BufReader<Box<dyn Transport>>,
    framing: Framing,
    pushback: Vec<u8>,
    position: usize,
}

impl Body {
    fn read_framed(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        match &mut self.framing {
            Framing::Empty => Ok(0),
            Framing::UntilClose => self.reader.read(buf),
            Framing::Length(remaining) => {
                if *remaining == 0 {
                    return Ok(0);
                }
                let limit = usize::try_from(*remaining)
                    .unwrap_or(usize::MAX)
                    .min(buf.len());
                let count = self.reader.read(buf.get_mut(..limit).unwrap_or(&mut []))?;
                if count == 0 {
                    *remaining = 0;
                } else {
                    *remaining = remaining.saturating_sub(count as u64);
                }
                Ok(count)
            }
            Framing::Chunked(state) => loop {
                match *state {
                    ChunkState::Done => return Ok(0),
                    ChunkState::Size => {
                        let line = read_line_limited(&mut self.reader, 128, None)?
                            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
                        let text = std::str::from_utf8(&line)
                            .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
                        let digits = text.split(';').next().unwrap_or("").trim();
                        if digits.is_empty() || digits.len() > 15 {
                            return Err(io::Error::from(io::ErrorKind::InvalidData));
                        }
                        let size = u64::from_str_radix(digits, 16)
                            .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
                        if size == 0 {
                            for _ in 0..64 {
                                match read_line_limited(&mut self.reader, 1024, None)? {
                                    Some(line) if !line.is_empty() => continue,
                                    _ => break,
                                }
                            }
                            *state = ChunkState::Done;
                            return Ok(0);
                        }
                        *state = ChunkState::Data(size);
                    }
                    ChunkState::Data(remaining) => {
                        let limit = usize::try_from(remaining)
                            .unwrap_or(usize::MAX)
                            .min(buf.len());
                        let count = self.reader.read(buf.get_mut(..limit).unwrap_or(&mut []))?;
                        if count == 0 {
                            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
                        }
                        let left = remaining.saturating_sub(count as u64);
                        *state = if left == 0 {
                            ChunkState::DataEnd
                        } else {
                            ChunkState::Data(left)
                        };
                        return Ok(count);
                    }
                    ChunkState::DataEnd => match read_line_limited(&mut self.reader, 16, None)? {
                        Some(line) if line.is_empty() => *state = ChunkState::Size,
                        Some(_) => return Err(io::Error::from(io::ErrorKind::InvalidData)),
                        None => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
                    },
                }
            },
        }
    }

    /// Makes up to `want` bytes available without consuming them and returns what
    /// is buffered. Once some bytes have arrived it waits at most `grace` for the
    /// rest, so a short reply on a connection that stays open is still judged.
    fn peek(&mut self, want: usize, ctl: &Ctl, grace: Duration) -> io::Result<&[u8]> {
        while self.pushback.len() - self.position < want {
            let room = (want - (self.pushback.len() - self.position)).min(NET_CHUNK);
            let mut temp = vec![0u8; room];
            match self.read_framed(&mut temp) {
                Ok(0) => break,
                Ok(count) => {
                    self.pushback
                        .extend_from_slice(temp.get(..count).unwrap_or(&[]));
                    ctl.set_idle(grace);
                }
                Err(error)
                    if error.kind() == io::ErrorKind::TimedOut
                        && self.pushback.len() > self.position =>
                {
                    break;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(self.pushback.get(self.position..).unwrap_or(&[]))
    }
}

impl Read for Body {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let buffered = self.pushback.len() - self.position;
        if buffered > 0 {
            let count = buffered.min(buf.len());
            let source = self
                .pushback
                .get(self.position..self.position + count)
                .unwrap_or(&[]);
            if let Some(target) = buf.get_mut(..source.len()) {
                target.copy_from_slice(source);
            }
            self.position += count;
            if self.position >= self.pushback.len() {
                self.pushback.clear();
                self.position = 0;
            }
            return Ok(count);
        }
        self.read_framed(buf)
    }
}

// ---------------------------------------------------------------------------
// Client: connect, request, redirects, classification
// ---------------------------------------------------------------------------

fn production_tls() -> Option<Arc<ClientConfig>> {
    static TLS: OnceLock<Option<Arc<ClientConfig>>> = OnceLock::new();
    TLS.get_or_init(|| {
        let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        tls_config_with_roots(roots)
    })
    .clone()
}

fn tls_config_with_roots(roots: RootCertStore) -> Option<Arc<ClientConfig>> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .ok()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Some(Arc::new(config))
}

fn resolve(host: &str, port: u16, budget: Duration) -> Result<Vec<SocketAddr>, StreamError> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let (sender, receiver) = mpsc::channel();
    let name = host.to_owned();
    std::thread::Builder::new()
        .name("bs-dns".into())
        .spawn(move || {
            let result = (name.as_str(), port)
                .to_socket_addrs()
                .map(|addresses| addresses.take(8).collect::<Vec<_>>());
            let _ = sender.send(result);
        })
        .map_err(|_| net("Could not start the name lookup"))?;
    match receiver.recv_timeout(budget) {
        Ok(Ok(mut addresses)) if !addresses.is_empty() => {
            // IPv4 first: a network without IPv6 would otherwise wait out a timeout.
            addresses.sort_by_key(|address| address.is_ipv6());
            Ok(addresses)
        }
        Ok(_) => Err(net(format!("Could not find {host}"))),
        Err(_) => Err(net(format!("Looking up {host} took too long"))),
    }
}

fn build_request(url: &Url, icy: bool) -> String {
    let mut request = String::with_capacity(256);
    request.push_str("GET ");
    request.push_str(&url.target);
    request.push_str(" HTTP/1.1\r\nHost: ");
    request.push_str(&url.authority());
    request.push_str("\r\nUser-Agent: ");
    request.push_str(USER_AGENT);
    request.push_str("\r\nAccept: */*\r\nAccept-Encoding: identity\r\n");
    if icy {
        request.push_str("Icy-MetaData: 1\r\n");
    }
    request.push_str("Connection: close\r\n\r\n");
    request
}

fn content_type_of(head: &Head) -> String {
    head.get("content-type")
        .and_then(|value| value.split(';').next())
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_default()
}

/// One response whose head has been read and whose body is ready.
struct Fetch {
    head: Head,
    body: Body,
    /// A second handle to the socket, used to shut the connection down.
    handle: TcpStream,
    /// Address that produced this response (after redirects).
    url: Url,
}

enum Classified {
    Audio,
    Playlist(Vec<u8>),
}

/// An opened audio response and where it came from.
struct Media {
    fetch: Fetch,
    /// Address before redirects; reconnects start here.
    source: Url,
    content_type: String,
}

/// A live connection handed to the network thread.
struct Active {
    body: Body,
    handle: TcpStream,
    metaint: u64,
}

#[derive(Clone)]
struct Client {
    tls: Option<Arc<ClientConfig>>,
    timing: Timing,
}

impl Client {
    fn production() -> Client {
        Client {
            tls: production_tls(),
            timing: Timing::PRODUCTION,
        }
    }

    fn connect_tcp(
        &self,
        url: &Url,
        deadline: Instant,
        ctl: &Ctl,
    ) -> Result<TcpStream, StreamError> {
        let budget = self
            .timing
            .connect
            .min(deadline.saturating_duration_since(Instant::now()))
            .max(Duration::from_millis(1));
        let connect_deadline = Instant::now() + budget;
        let addresses = resolve(&url.host, url.port, budget)?;
        for address in addresses.into_iter().take(MAX_ADDRS) {
            if ctl.cancelled() {
                return Err(StreamError::Cancelled);
            }
            let left = connect_deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            if let Ok(socket) = TcpStream::connect_timeout(&address, left) {
                let _ = socket.set_nodelay(true);
                let _ = socket.set_read_timeout(Some(SOCKET_SLICE));
                let _ = socket.set_write_timeout(Some(WRITE_TIMEOUT));
                return Ok(socket);
            }
        }
        Err(net(format!("Could not connect to {}", url.host)))
    }

    /// Connects, sends the request and reads the response head.
    fn exchange(
        &self,
        url: &Url,
        icy: bool,
        deadline: Instant,
        ctl: &Arc<Ctl>,
    ) -> Result<Fetch, StreamError> {
        if ctl.cancelled() {
            return Err(StreamError::Cancelled);
        }
        let socket = self.connect_tcp(url, deadline, ctl)?;
        let handle = socket
            .try_clone()
            .map_err(|_| net(format!("Could not connect to {}", url.host)))?;
        let patient = Patient {
            inner: socket,
            ctl: ctl.clone(),
        };
        let transport: Box<dyn Transport> = if url.https {
            let config = self
                .tls
                .clone()
                .ok_or_else(|| net("Secure (https) connections are not available"))?;
            let name = ServerName::try_from(url.host.clone())
                .map_err(|_| unsupported("The address has an invalid host name"))?;
            let connection = ClientConnection::new(config, name)
                .map_err(|_| net(format!("The secure connection to {} failed", url.host)))?;
            Box::new(StreamOwned::new(connection, patient))
        } else {
            Box::new(patient)
        };
        let mut reader = BufReader::with_capacity(16 * 1024, transport);
        ctl.set_idle_until(self.timing.read, deadline);
        let request = build_request(url, icy);
        reader
            .get_mut()
            .write_all(request.as_bytes())
            .and_then(|()| reader.get_mut().flush())
            .map_err(|error| io_to_error(&url.host, &error))?;
        let head = read_head(&mut reader, deadline)?;
        let framing = framing_for(&head)?;
        ctl.set_idle(self.timing.read);
        Ok(Fetch {
            head,
            body: Body {
                reader,
                framing,
                pushback: Vec::new(),
                position: 0,
            },
            handle,
            url: url.clone(),
        })
    }

    /// Follows redirects (at most [`MAX_REDIRECTS`]) until a 2xx response.
    fn fetch(
        &self,
        start: &Url,
        icy: bool,
        deadline: Instant,
        ctl: &Arc<Ctl>,
    ) -> Result<Fetch, StreamError> {
        let mut url = start.clone();
        let mut hops = 0u32;
        loop {
            let fetch = self.exchange(&url, icy, deadline, ctl)?;
            match fetch.head.status {
                301 | 302 | 303 | 307 | 308 => {
                    hops += 1;
                    if hops > MAX_REDIRECTS {
                        return Err(net(format!("Too many redirects from {}", start.host)));
                    }
                    let location = fetch
                        .head
                        .get("location")
                        .ok_or_else(|| net("The server redirected without saying where"))?;
                    let next = url.join(location)?;
                    if url.https && !next.https {
                        return Err(net(
                            "A redirect from a secure to an insecure address was refused",
                        ));
                    }
                    url = next;
                }
                status if status >= 400 => return Err(StreamError::Http(status)),
                status if (200..300).contains(&status) => return Ok(fetch),
                _ => return Err(net("The server sent an unexpected response")),
            }
        }
    }

    /// Reads a playlist body (at most [`MAX_PLAYLIST_BYTES`]).
    fn read_playlist(fetch: &mut Fetch) -> Result<Vec<u8>, StreamError> {
        let mut bytes = Vec::new();
        let mut limited = (&mut fetch.body).take(MAX_PLAYLIST_BYTES as u64);
        limited
            .read_to_end(&mut bytes)
            .map_err(|error| io_to_error(&fetch.url.host, &error))?;
        Ok(bytes)
    }

    /// Decides whether a 2xx response is audio or a playlist (or refuses it).
    fn classify(
        &self,
        fetch: &mut Fetch,
        deadline: Instant,
        ctl: &Ctl,
    ) -> Result<Classified, StreamError> {
        let kind = classify_content_type(&content_type_of(&fetch.head));
        match kind {
            Kind::Web => return Err(unsupported(MSG_WEB_PAGE)),
            Kind::Opus => return Err(unsupported(MSG_OPUS)),
            Kind::Playlist => return Self::read_playlist(fetch).map(Classified::Playlist),
            Kind::Audio | Kind::Unknown => {}
        }
        ctl.set_idle_until(self.timing.read, deadline);
        let text = {
            let host = fetch.url.host.clone();
            let peeked = fetch
                .body
                .peek(PEEK_BYTES, ctl, PEEK_GRACE)
                .map_err(|error| io_to_error(&host, &error))?;
            sniff_text(peeked, kind == Kind::Unknown)
        };
        ctl.set_idle(self.timing.read);
        match text {
            Some(TextKind::Html) => Err(unsupported(MSG_WEB_PAGE)),
            Some(TextKind::Xml) => Err(unsupported("This playlist type is not supported")),
            Some(TextKind::Hls) => Err(unsupported(MSG_HLS)),
            Some(TextKind::Pls | TextKind::M3u) => {
                Self::read_playlist(fetch).map(Classified::Playlist)
            }
            None => Ok(Classified::Audio),
        }
    }

    /// Fetches `start`, following playlists, until an audio response is found.
    fn open_media(
        &self,
        start: &Url,
        deadline: Instant,
        ctl: &Arc<Ctl>,
    ) -> Result<Media, StreamError> {
        let mut url = start.clone();
        let mut followed = 0u32;
        loop {
            let mut fetch = self.fetch(&url, true, deadline, ctl)?;
            match self.classify(&mut fetch, deadline, ctl)? {
                Classified::Audio => {
                    let content_type = content_type_of(&fetch.head);
                    return Ok(Media {
                        fetch,
                        source: url,
                        content_type,
                    });
                }
                Classified::Playlist(bytes) => {
                    followed += 1;
                    if followed > MAX_PLAYLIST_LEVELS {
                        return Err(unsupported(
                            "This playlist points to too many other playlists",
                        ));
                    }
                    url = first_valid_url(&parse_playlist(&bytes)?)?;
                }
            }
        }
    }

    /// Playlist expansion without opening the stream itself.
    #[allow(dead_code)] // only `resolve_playlist_url` (not used by the app) calls it
    fn resolve_url(&self, start: &Url) -> Result<String, StreamError> {
        let deadline = Instant::now() + self.timing.open;
        let ctl = Ctl::new(Arc::new(AtomicBool::new(false)), self.timing.read);
        let mut url = start.clone();
        let mut followed = 0u32;
        loop {
            let mut fetch = self.fetch(&url, false, deadline, &ctl)?;
            match self.classify(&mut fetch, deadline, &ctl)? {
                Classified::Audio => return Ok(url.to_url_string()),
                Classified::Playlist(bytes) => {
                    followed += 1;
                    if followed > MAX_PLAYLIST_LEVELS {
                        return Err(unsupported(
                            "This playlist points to too many other playlists",
                        ));
                    }
                    let entry = first_valid_url(&parse_playlist(&bytes)?)?;
                    if !entry.looks_like_playlist() {
                        return Ok(entry.to_url_string());
                    }
                    url = entry;
                }
            }
        }
    }

    fn open(
        &self,
        input: &str,
        preload_ms: u32,
        cancel: Arc<AtomicBool>,
    ) -> Result<LiveStream, StreamError> {
        let start = Url::parse(input)?;
        let deadline = Instant::now() + self.timing.open;
        let ctl = Ctl::new(cancel.clone(), self.timing.read);
        let Media {
            fetch,
            source,
            content_type,
        } = self.open_media(&start, deadline, &ctl)?;
        let metaint = parse_metaint(&fetch.head)?;
        let kbps = parse_bitrate(&fetch.head);
        let info = StreamInfo {
            name: fetch
                .head
                .get("icy-name")
                .map(|value| clean_text(value, MAX_NAME_CHARS))
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| fetch.url.host.clone()),
            genre: fetch
                .head
                .get("icy-genre")
                .map(|value| clean_text(value, MAX_NAME_CHARS))
                .unwrap_or_default(),
            bitrate_kbps: kbps,
            content_type: content_type.clone(),
            url: fetch.url.to_url_string(),
        };
        let capacity = ring_capacity(kbps);
        let shared = Arc::new(Shared::new(capacity, cancel.clone()));
        let active = Active {
            body: fetch.body,
            handle: fetch.handle,
            metaint,
        };
        let thread_client = self.clone();
        let thread_shared = shared.clone();
        let thread_ctl = ctl.clone();
        std::thread::Builder::new()
            .name("bs-stream".into())
            .spawn(move || run_network(thread_client, thread_shared, thread_ctl, source, active))
            .map_err(|_| net("Could not start the stream reader"))?;

        let trusted = classify_content_type(&content_type) == Kind::Audio;
        let target = preload_target(kbps, preload_ms, capacity);
        match wait_for_audio(&shared, target, deadline) {
            Ok(prefix) => {
                if let Err(error) = check_audio_prefix(&prefix, trusted) {
                    shared.close();
                    return Err(error);
                }
            }
            Err(error) => {
                shared.close();
                return Err(error);
            }
        }
        Ok(LiveStream {
            shared,
            info: Arc::new(info),
        })
    }
}

/// `icy-metaint`: 0 when absent or unreadable; an error when implausibly small.
fn parse_metaint(head: &Head) -> Result<u64, StreamError> {
    match head
        .get("icy-metaint")
        .and_then(|value| value.trim().parse::<u64>().ok())
    {
        None | Some(0) => Ok(0),
        Some(value) if value >= MIN_METAINT => Ok(value),
        Some(_) => Err(net("The station sent an invalid metadata interval")),
    }
}

/// Station bitrate from `icy-br` (or Icecast's `ice-audio-info`), when plausible.
fn parse_bitrate(head: &Head) -> Option<u32> {
    let from_number = |text: &str| {
        text.split(|c: char| !c.is_ascii_digit())
            .find(|part| !part.is_empty())
            .and_then(|part| part.parse::<u32>().ok())
    };
    let value = head.get("icy-br").and_then(from_number).or_else(|| {
        let info = head.get("ice-audio-info")?;
        let index = info.find("bitrate=")?;
        from_number(info.get(index + "bitrate=".len()..)?)
    })?;
    (8..=2048).contains(&value).then_some(value)
}

/// Ring buffer size: about eight seconds of audio, between 256 KiB and 4 MiB.
fn ring_capacity(kbps: Option<u32>) -> usize {
    let kbps = u64::from(kbps.unwrap_or(DEFAULT_KBPS).clamp(8, 2048));
    let bytes = kbps * 125 * RING_SECONDS;
    usize::try_from(bytes)
        .unwrap_or(MAX_RING_BYTES)
        .clamp(MIN_RING_BYTES, MAX_RING_BYTES)
}

/// Bytes to buffer before `open_stream` returns.
fn preload_target(kbps: Option<u32>, preload_ms: u32, capacity: usize) -> usize {
    let kbps = u64::from(kbps.unwrap_or(DEFAULT_KBPS).clamp(8, 2048));
    let bytes = kbps * 125 * u64::from(preload_ms) / 1000;
    usize::try_from(bytes)
        .unwrap_or(usize::MAX)
        .clamp(4 * 1024, (capacity / 2).max(4 * 1024))
}

// ---------------------------------------------------------------------------
// Shared ring buffer
// ---------------------------------------------------------------------------

struct Inner {
    buf: VecDeque<u8>,
    cap: usize,
    /// Audio bytes ever pushed / ever read (title positions are in these units).
    pushed: u64,
    consumed: u64,
    titles: VecDeque<(u64, String)>,
    title: Option<String>,
    status: StreamStatus,
    /// The network thread is gone; remaining bytes can still be read.
    finished: bool,
    conn: Option<TcpStream>,
    /// Connections made so far (1 after the first; +1 for every reconnect).
    connections: u32,
}

impl Inner {
    fn apply_titles(&mut self) {
        while self
            .titles
            .front()
            .is_some_and(|(position, _)| *position <= self.consumed)
        {
            if let Some((_, title)) = self.titles.pop_front() {
                self.title = if title.is_empty() { None } else { Some(title) };
            }
        }
    }
}

struct Shared {
    inner: Mutex<Inner>,
    readable: Condvar,
    writable: Condvar,
    closed: Arc<AtomicBool>,
}

impl Shared {
    fn new(cap: usize, closed: Arc<AtomicBool>) -> Shared {
        Shared {
            inner: Mutex::new(Inner {
                buf: VecDeque::with_capacity(cap),
                cap,
                pushed: 0,
                consumed: 0,
                titles: VecDeque::new(),
                title: None,
                status: StreamStatus::Connecting,
                finished: false,
                conn: None,
                connections: 0,
            }),
            readable: Condvar::new(),
            writable: Condvar::new(),
            closed,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn wait<'a>(
        &self,
        condvar: &Condvar,
        guard: MutexGuard<'a, Inner>,
        timeout: Duration,
    ) -> MutexGuard<'a, Inner> {
        match condvar.wait_timeout(guard, timeout) {
            Ok((guard, _)) => guard,
            Err(poison) => poison.into_inner().0,
        }
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Stops the stream: readers see the end at once, the network thread is told
    /// to stop and its connection is shut down.
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let connection = {
            let mut inner = self.lock();
            inner.buf = VecDeque::new();
            inner.titles.clear();
            inner.conn.take()
        };
        if let Some(connection) = connection {
            let _ = connection.shutdown(Shutdown::Both);
        }
        self.readable.notify_all();
        self.writable.notify_all();
    }

    /// Registers the connection that `close` must shut down.
    fn set_conn(&self, connection: TcpStream) {
        let previous = {
            let mut inner = self.lock();
            inner.connections = inner.connections.saturating_add(1);
            inner.conn.replace(connection)
        };
        if let Some(previous) = previous {
            let _ = previous.shutdown(Shutdown::Both);
        }
        if self.is_closed() {
            let connection = self.lock().conn.take();
            if let Some(connection) = connection {
                let _ = connection.shutdown(Shutdown::Both);
            }
        }
    }

    fn set_status(&self, status: StreamStatus) {
        let mut inner = self.lock();
        inner.status = status;
        drop(inner);
        self.readable.notify_all();
    }

    fn fail(&self, message: String) {
        let mut inner = self.lock();
        inner.status = StreamStatus::Failed(message);
        inner.finished = true;
        drop(inner);
        self.readable.notify_all();
    }

    /// Adds audio (blocking while the buffer is full) and registers the titles
    /// found in it. Returns false when the stream was closed.
    fn push(&self, audio: &[u8], titles: &[(usize, String)]) -> bool {
        let mut inner = self.lock();
        let base = inner.pushed;
        for (offset, title) in titles {
            inner
                .titles
                .push_back((base.saturating_add(*offset as u64), title.clone()));
            if inner.titles.len() > MAX_PENDING_TITLES
                && let Some((_, title)) = inner.titles.pop_front()
            {
                inner.title = if title.is_empty() { None } else { Some(title) };
            }
        }
        inner.apply_titles();
        let mut rest = audio;
        while !rest.is_empty() {
            if self.is_closed() {
                return false;
            }
            let space = inner.cap.saturating_sub(inner.buf.len());
            if space == 0 {
                inner = self.wait(&self.writable, inner, POLL);
                continue;
            }
            let (now, later) = rest.split_at(space.min(rest.len()));
            inner.buf.extend(now.iter().copied());
            inner.pushed = inner.pushed.saturating_add(now.len() as u64);
            rest = later;
            if matches!(
                inner.status,
                StreamStatus::Connecting | StreamStatus::Reconnecting { .. }
            ) {
                inner.status = StreamStatus::Playing;
            }
            self.readable.notify_all();
        }
        !self.is_closed()
    }

    /// Sleeps for `duration` unless the stream is closed first (returns false).
    fn sleep(&self, duration: Duration) -> bool {
        let end = Instant::now() + duration;
        let mut inner = self.lock();
        loop {
            if self.is_closed() {
                return false;
            }
            let now = Instant::now();
            if now >= end {
                return true;
            }
            inner = self.wait(&self.writable, inner, (end - now).min(POLL));
        }
    }

    fn read_into(&self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let mut inner = self.lock();
        loop {
            if self.is_closed() {
                return Ok(0);
            }
            if !inner.buf.is_empty() {
                let count = out.len().min(inner.buf.len());
                {
                    let (first, second) = inner.buf.as_slices();
                    let from_first = count.min(first.len());
                    if let (Some(target), Some(source)) =
                        (out.get_mut(..from_first), first.get(..from_first))
                    {
                        target.copy_from_slice(source);
                    }
                    if count > from_first
                        && let (Some(target), Some(source)) = (
                            out.get_mut(from_first..count),
                            second.get(..count - from_first),
                        )
                    {
                        target.copy_from_slice(source);
                    }
                }
                inner.buf.drain(..count);
                inner.consumed = inner.consumed.saturating_add(count as u64);
                inner.apply_titles();
                drop(inner);
                self.writable.notify_all();
                return Ok(count);
            }
            if inner.finished {
                return Ok(0);
            }
            inner = self.wait(&self.readable, inner, POLL);
        }
    }
}

/// Marks the network thread as gone, also when it panics.
struct FinishGuard(Arc<Shared>);

impl Drop for FinishGuard {
    fn drop(&mut self) {
        let mut inner = self.0.lock();
        inner.finished = true;
        if std::thread::panicking() && !matches!(inner.status, StreamStatus::Failed(_)) {
            inner.status = StreamStatus::Failed("The stream reader stopped unexpectedly".into());
        }
        inner.conn = None;
        drop(inner);
        self.0.readable.notify_all();
        self.0.writable.notify_all();
    }
}

/// Waits until `target` bytes are buffered (or, once the deadline has passed, at
/// least [`MIN_PROBE_BYTES`]) and returns the start of the buffered audio.
fn wait_for_audio(
    shared: &Shared,
    target: usize,
    deadline: Instant,
) -> Result<Vec<u8>, StreamError> {
    let mut inner = shared.lock();
    loop {
        if shared.is_closed() {
            return Err(StreamError::Cancelled);
        }
        if let StreamStatus::Failed(message) = &inner.status {
            return Err(net(message.clone()));
        }
        let ended = inner.finished || matches!(inner.status, StreamStatus::Reconnecting { .. });
        let timed_out = Instant::now() >= deadline;
        if inner.buf.len() >= target {
            break;
        }
        if ended || timed_out {
            if inner.buf.len() >= MIN_PROBE_BYTES {
                break;
            }
            return Err(net(if ended {
                "The station stopped sending audio"
            } else {
                "The station did not send audio in time"
            }));
        }
        let left = deadline.saturating_duration_since(Instant::now());
        inner = shared.wait(&shared.readable, inner, left.min(POLL));
    }
    Ok(inner.buf.iter().take(SNIFF_WINDOW).copied().collect())
}

// ---------------------------------------------------------------------------
// Network thread
// ---------------------------------------------------------------------------

/// Reads one connection until it ends. Returns the bytes received and how long
/// the connection lasted.
fn pump(shared: &Shared, ctl: &Ctl, timing: Timing, active: Active) -> (u64, Duration) {
    let Active {
        mut body,
        handle,
        metaint,
    } = active;
    shared.set_conn(handle);
    ctl.set_idle(timing.read);
    let started = Instant::now();
    let mut received = 0u64;
    let mut demux = IcyDemux::new(metaint);
    let mut chunk = vec![0u8; NET_CHUNK];
    let mut audio: Vec<u8> = Vec::with_capacity(NET_CHUNK);
    let mut titles: Vec<(usize, String)> = Vec::new();
    loop {
        if shared.is_closed() {
            break;
        }
        match body.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => {
                audio.clear();
                titles.clear();
                demux.feed(chunk.get(..count).unwrap_or(&[]), &mut audio, &mut titles);
                received = received.saturating_add(count as u64);
                if !shared.push(&audio, &titles) {
                    break;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    (received, started.elapsed())
}

enum Reconnect {
    /// Try again after the next delay.
    Retry,
    /// Give up for good.
    Fatal(String),
    /// Closed while connecting.
    Stop,
}

fn reconnect(client: &Client, source: &Url, ctl: &Arc<Ctl>) -> Result<Active, Reconnect> {
    let deadline = Instant::now() + client.timing.open;
    ctl.set_idle(client.timing.read);
    let outcome = client
        .fetch(source, true, deadline, ctl)
        .and_then(
            |mut fetch| match client.classify(&mut fetch, deadline, ctl)? {
                Classified::Audio => Ok(fetch),
                // A station that turned into an HLS playlist will not come back as audio.
                Classified::Playlist(bytes) => Err(match parse_playlist(&bytes) {
                    Err(StreamError::Unsupported(message)) if message == MSG_HLS => {
                        StreamError::Unsupported(message)
                    }
                    _ => net("The server did not send audio"),
                }),
            },
        );
    match outcome {
        Ok(fetch) => match parse_metaint(&fetch.head) {
            Ok(metaint) => Ok(Active {
                body: fetch.body,
                handle: fetch.handle,
                metaint,
            }),
            Err(_) => Err(Reconnect::Retry),
        },
        Err(StreamError::Cancelled) => Err(Reconnect::Stop),
        // A station that now sends a format we cannot play will not recover.
        Err(StreamError::Unsupported(message)) if message == MSG_HLS || message == MSG_OPUS => {
            Err(Reconnect::Fatal(message))
        }
        // Errors, error pages, outages: keep trying until the user stops.
        Err(_) => Err(Reconnect::Retry),
    }
}

fn run_network(client: Client, shared: Arc<Shared>, ctl: Arc<Ctl>, source: Url, first: Active) {
    let _guard = FinishGuard(shared.clone());
    let mut backoff = Backoff::with_schedule_ms(client.timing.backoff_ms);
    let mut active = Some(first);
    loop {
        if shared.is_closed() {
            return;
        }
        if let Some(connection) = active.take() {
            let (bytes, lasted) = pump(&shared, &ctl, client.timing, connection);
            if shared.is_closed() {
                return;
            }
            backoff.note_session(lasted, bytes);
        }
        let delay = backoff.next_delay();
        shared.set_status(StreamStatus::Reconnecting {
            attempt: backoff.attempt(),
        });
        if !shared.sleep(delay) {
            return;
        }
        match reconnect(&client, &source, &ctl) {
            Ok(connection) => active = Some(connection),
            Err(Reconnect::Retry) => {}
            Err(Reconnect::Fatal(message)) => {
                shared.fail(message);
                return;
            }
            Err(Reconnect::Stop) => return,
        }
    }
}

// ---------------------------------------------------------------------------
// Public stream handle and entry points
// ---------------------------------------------------------------------------

/// A live stream as a byte reader. Blocks while the buffer is empty; ends only
/// after [`LiveStream::close`], after the stream failed for good, or when dropped.
/// Not seekable (wrap in [`NonSeekable`] for a decoder).
pub struct LiveStream {
    shared: Arc<Shared>,
    info: Arc<StreamInfo>,
}

impl LiveStream {
    /// Latest ICY title that the listener has reached; `None` until the first
    /// non-empty one (and again after the station clears it).
    #[allow(dead_code)] // the app reads the title through `StreamHandle`
    pub fn title(&self) -> Option<String> {
        self.shared.lock().title.clone()
    }

    #[allow(dead_code)] // the app reads the state through `StreamHandle`
    pub fn status(&self) -> StreamStatus {
        self.shared.lock().status.clone()
    }

    /// Connections made so far: 1 after the first, +1 for every reconnect. A change
    /// means the byte stream was spliced (a decoder may need to be rebuilt).
    #[allow(dead_code)] // the decoder thread reads it through `StreamHandle`
    pub fn connections(&self) -> u32 {
        self.shared.lock().connections
    }

    pub fn info(&self) -> &StreamInfo {
        &self.info
    }

    /// Stops the network thread and drops the connection; readers see the end.
    pub fn close(&self) {
        self.shared.close();
    }

    /// A cloneable view for the UI while the reader itself is owned by a decoder.
    pub fn handle(&self) -> StreamHandle {
        StreamHandle {
            shared: self.shared.clone(),
            info: self.info.clone(),
        }
    }
}

impl Read for LiveStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.shared.read_into(buf)
    }
}

impl Drop for LiveStream {
    fn drop(&mut self) {
        self.shared.close();
    }
}

/// Cloneable, thread-safe view of a [`LiveStream`]: title, status, info and close.
#[derive(Clone)]
pub struct StreamHandle {
    shared: Arc<Shared>,
    info: Arc<StreamInfo>,
}

impl StreamHandle {
    pub fn title(&self) -> Option<String> {
        self.shared.lock().title.clone()
    }

    pub fn status(&self) -> StreamStatus {
        self.shared.lock().status.clone()
    }

    pub fn connections(&self) -> u32 {
        self.shared.lock().connections
    }

    pub fn info(&self) -> &StreamInfo {
        &self.info
    }

    pub fn close(&self) {
        self.shared.close();
    }
}

/// Makes any reader look like a `Read + Seek` source that cannot seek, which is
/// what a decoder needs for a live stream. `Seek::seek` fails with
/// `ErrorKind::Unsupported` except `SeekFrom::Current(0)`, which reports 0
/// (the position of a live stream is unknown).
pub struct NonSeekable<R>(pub R);

impl<R: Read> Read for NonSeekable<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl<R> Seek for NonSeekable<R> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        match position {
            SeekFrom::Current(0) => Ok(0),
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "live streams cannot be seeked",
            )),
        }
    }
}

/// Opens a live stream. Blocks (at most 15 s) until the response headers are in
/// and about `preload_ms` of audio is buffered, so call it from a worker thread.
/// Playlist addresses (`.pls`, `.m3u`) are followed to their first entry.
#[allow(dead_code)] // the app and the engine use `open_stream_with_cancel`
pub fn open_stream(url: &str, preload_ms: u32) -> Result<LiveStream, StreamError> {
    open_stream_with_cancel(url, preload_ms, Arc::new(AtomicBool::new(false)))
}

/// Like [`open_stream`], but setting `cancel` stops the open (`Cancelled`) and,
/// once it succeeded, the stream itself (the flag is shared with `close`).
pub fn open_stream_with_cancel(
    url: &str,
    preload_ms: u32,
    cancel: Arc<AtomicBool>,
) -> Result<LiveStream, StreamError> {
    Client::production().open(url, preload_ms, cancel)
}

/// `.pls` / `.m3u` / `.m3u8`-as-plain-list to the first http(s) entry, following
/// up to three nested playlists. An address that is not a playlist is returned
/// unchanged. The stream itself is not contacted.
#[allow(dead_code)] // `open_stream_with_cancel` follows playlists itself; nothing needs this alone
pub fn resolve_playlist_url(url: &str) -> Result<String, StreamError> {
    let start = Url::parse(url)?;
    Client::production().resolve_url(&start)
}
