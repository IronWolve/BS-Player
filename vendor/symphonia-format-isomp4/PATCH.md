# Local audio-container patches

Based on symphonia-format-isomp4 0.5.5 (upstream crate SHA-256
`243739585d11f81daf8dac8d9f3d18cc7898f6c09a259675fc364b382c30e0a5`).

- Normalize big-endian `dOps` fields to an Opus identification header and expose
  the decoded sample rate/channel layout.
- Expose selected-track timing and frame trims for simple edit lists. An initial
  empty movie edit is excluded from the audio-only presentation duration.
- Seeking the selected track survives EOF in shorter, unselected tracks.
- Bound edit/sample tables and packet allocations; check packet file extents
  before allocation. Unknown/non-audio sample entries are skipped.

These changes support audio playback, not video rendering. Upstream source
headers and README are retained. See the fixture generator and focused
`playback::container_tests` in the parent application.

Timing reference: [Opus in ISO Base Media File Format](https://www.opus-codec.org/docs/opus_in_isobmff.html).
