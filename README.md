# BS Player

<p align="center">
  <img src="assets/bs-player-logo.png" alt="BS Player" width="260">
</p>

BS Player is a music, audiobook and internet-radio player built in Rust with a
focus on speed, responsiveness and low memory usage. Your library, playlists,
listening positions and bookmarks stay in a portable profile beside the app.

Supports **Windows 10/11 (64-bit)**.

## Screenshots

Captured with generated demo entries, silent test files and demo artwork.

![BS Player library and playback window](assets/screenshots/player.png)

![Chapter picker](assets/screenshots/chapters.png)

| Deep Plum | Polar Slate |
| --- | --- |
| <img src="assets/screenshots/theme-deep-plum.png" alt="Deep Plum theme" width="420"> | <img src="assets/screenshots/theme-polar-slate.png" alt="Polar Slate theme" width="420"> |
| **Warm Charcoal** | **Parchment** |
| <img src="assets/screenshots/theme-warm-charcoal.png" alt="Warm Charcoal theme" width="420"> | <img src="assets/screenshots/theme-parchment.png" alt="Parchment theme" width="420"> |

## Features

- Music and audiobook library with search, playlists, queue, favorites and
  28 configurable, sortable library columns.
- Internet radio with saved stations, HTTP/HTTPS streams, M3U/PLS playlists,
  live track titles and automatic reconnects.
- Remembers listening position, bookmarks and playback speed; supports chapters,
  sleep timer, silence skipping and speed changes with pitch preserved.
- Gapless playback, ten-band equalizer, ReplayGain, shuffle, repeat and A–B
  repeat.
- Light, dark and system appearance, with named color themes.
- Plays MP3, FLAC, Ogg Vorbis, Opus, WAV, AIFF, AAC and supported M4A/M4B audio.
  MP4 and MKV containers are read as audio only; video and subtitles are not
  played. Dolby, DTS and WMA audio, and surround-channel Opus, are unsupported.

## Windows setup

Get the Windows package from
[GitHub Releases](https://github.com/IronWolve/BS-Player/releases). Extract it
to a writable folder and run `bs-player.exe`. Keep the adjacent `profile/`
folder when updating; it contains your portable settings and listening data.
The Windows application is unsigned, so Windows may show a security prompt.

## License

BS Player is licensed under [Apache License 2.0](LICENSE).
