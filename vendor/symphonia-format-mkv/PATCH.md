# Local audio-container patches

Based on symphonia-format-mkv 0.5.5 (upstream crate SHA-256
`122d786d2c43a49beb6f397551b4a050d8229eaa54c7ddf9ee4b98899b8742d0`).

- Preserve codec delay, seek preroll and discard padding in decoded-frame units.
  Keep raw timestamps so the audio adapter applies presentation offsets once.
- Expose selected-audio timing estimates. Movie duration is not substituted for
  audio duration in multiplexed files without selected-track evidence.
- Clear stale seek queues and retain the nearest preceding packet when packet
  duration is unknown, including laced blocks.
- Bound EBML/block allocations and PCM frame capacity; skip non-audio simple
  block payloads before allocation.

These changes support audio playback, not video rendering. Upstream source
headers and README are retained. See the fixture generator and focused
`playback::container_tests` in the parent application.

Timing reference: [Matroska specification notes](https://www.matroska.org/technical/notes.html).
