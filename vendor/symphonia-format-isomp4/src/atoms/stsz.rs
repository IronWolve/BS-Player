// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::Result;
use symphonia_core::io::ReadBytes;

use crate::atoms::{Atom, AtomHeader};

/// Bound variable sample-size tables before reserving attacker-controlled memory.
const MAX_SAMPLE_COUNT: u32 = 10_000_000;

#[derive(Debug)]
pub enum SampleSize {
    Constant(u32),
    Variable(Vec<u32>),
}

/// Sample Size Atom
#[allow(dead_code)]
#[derive(Debug)]
pub struct StszAtom {
    /// Atom header.
    header: AtomHeader,
    /// The total number of samples.
    pub sample_count: u32,
    /// A vector of `sample_count` sample sizes, or a constant size for all samples.
    pub sample_sizes: SampleSize,
}

impl Atom for StszAtom {
    fn header(&self) -> AtomHeader {
        self.header
    }

    fn read<B: ReadBytes>(reader: &mut B, header: AtomHeader) -> Result<Self> {
        let (_, _) = AtomHeader::read_extra(reader)?;

        let sample_size = reader.read_be_u32()?;
        let sample_count = reader.read_be_u32()?;

        if sample_count > MAX_SAMPLE_COUNT {
            return Err(symphonia_core::errors::Error::DecodeError(
                "isomp4: sample-size table exceeds the safety limit",
            ));
        }

        let sample_sizes = if sample_size == 0 {
            if 12u64.saturating_add(u64::from(sample_count) * 4) > header.data_len {
                return Err(symphonia_core::errors::Error::DecodeError(
                    "isomp4: sample-size table exceeds its atom",
                ));
            }
            let mut entries = Vec::with_capacity(sample_count as usize);

            for _ in 0..sample_count {
                entries.push(reader.read_be_u32()?);
            }

            SampleSize::Variable(entries)
        }
        else {
            SampleSize::Constant(sample_size)
        };

        Ok(StszAtom { header, sample_count, sample_sizes })
    }
}
