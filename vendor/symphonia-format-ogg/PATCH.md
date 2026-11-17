# Local patch provenance

This source is based on `symphonia-format-ogg` 0.5.5 from the existing Cargo
registry cache. The corresponding `Cargo.lock` registry checksum is
`2b4955c67c1ed3aa8ae8428d04ca8397fbef6a19b2b051e73b5da8b1435639cb`.

The only code change is in `src/mappings/opus.rs`: Code 3's frame-count field
`M` is six bits in RFC 6716. The upstream mask kept only five bits, undercounting
packets whose high `M` bit is set. Existing upstream source headers are preserved.
