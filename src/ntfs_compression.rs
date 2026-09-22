//! NTFS LZNT1 compression units, shared by extraction and nested-image reads.
//!
//! A unit is all sparse, all allocated (stored verbatim), or allocated clusters
//! followed by sparse padding (LZNT1). Runs can cross units and MFT records.
//! Format: Microsoft ATTRIBUTE_RECORD_HEADER and MS-XCA section 2.5.

use anyhow::{Context, Result, anyhow, bail, ensure};
use ntfs::{
    NtfsAttribute, NtfsAttributeType, NtfsFile, attribute_value::NtfsAttributeValue,
    structured_values::NtfsAttributeList,
};
use std::collections::BTreeSet;
use std::io::{Read, Seek, SeekFrom};

const CHUNK: usize = 4096;
const MAX_RUNS: usize = 1_000_000;

#[derive(Debug)]
struct Run {
    start: u64,
    end: u64,
    physical: Option<u64>,
}

/// An owned run map; at most one 64 KiB decoded unit is cached.
pub(crate) struct CompressedStream {
    runs: Vec<Run>,
    length: u64,
    initialized: u64,
    unit_size: usize,
    cache: Vec<u8>,
    cached_unit: Option<u64>,
}

impl CompressedStream {
    pub(crate) fn open<R: Read + Seek>(file: &NtfsFile<'_>, reader: &mut R) -> Result<Self> {
        let cluster = u64::from(file.ntfs().cluster_size());
        ensure!(
            cluster.is_power_of_two() && (512..=4096).contains(&cluster),
            "unsupported NTFS compression cluster size {cluster}"
        );
        let mut extents = Vec::new();
        let mut seen = BTreeSet::new();
        let mut run_count = 0;
        for item in file.attributes_raw() {
            let attribute = item?;
            if attribute.ty()? == NtfsAttributeType::AttributeList {
                let list: NtfsAttributeList<'_, '_> = attribute.structured_value(reader)?;
                let mut entries = list.entries();
                let mut count = 0;
                while let Some(entry) = entries.next(reader) {
                    count += 1;
                    ensure!(
                        count <= MAX_RUNS,
                        "NTFS attribute list exceeds the safety limit"
                    );
                    let entry = entry?;
                    if entry.ty()? != NtfsAttributeType::Data || entry.name_length() != 0 {
                        continue;
                    }
                    let record = entry.to_file(file.ntfs(), reader)?;
                    let attribute = entry.to_attribute(&record)?;
                    Self::add_extent(
                        &attribute,
                        reader,
                        cluster,
                        &mut seen,
                        &mut extents,
                        &mut run_count,
                    )?;
                }
            } else if attribute.ty()? == NtfsAttributeType::Data && attribute.name_length() == 0 {
                Self::add_extent(
                    &attribute,
                    reader,
                    cluster,
                    &mut seen,
                    &mut extents,
                    &mut run_count,
                )?;
            }
        }
        extents.sort_by_key(|extent| extent.start);
        let first = extents
            .first()
            .ok_or_else(|| anyhow!("compressed NTFS stream has no data extents"))?;
        ensure!(
            first.start == 0,
            "compressed NTFS stream is missing its first extent"
        );
        let length = first.length;
        let initialized = first.initialized;
        ensure!(
            initialized <= length,
            "NTFS initialized length exceeds file length"
        );
        let unit_size = usize::try_from(cluster * 16)?;
        let mut runs = Vec::new();
        let mut end = 0;
        for extent in extents {
            ensure!(
                extent.start == end,
                "NTFS compressed extents overlap or have a gap at byte {end}"
            );
            end = extent.end;
            ensure!(
                runs.len().saturating_add(extent.runs.len()) <= MAX_RUNS,
                "NTFS run count exceeds the safety limit"
            );
            runs.extend(extent.runs);
        }
        let required = initialized
            .div_ceil(unit_size as u64)
            .checked_mul(unit_size as u64)
            .context("NTFS compression length overflow")?;
        ensure!(
            end >= required,
            "NTFS compressed run map ends before initialized data"
        );
        Ok(Self {
            runs,
            length,
            initialized,
            unit_size,
            cache: vec![0; unit_size],
            cached_unit: None,
        })
    }

    fn add_extent<R: Read + Seek>(
        attribute: &NtfsAttribute<'_, '_>,
        reader: &mut R,
        cluster: u64,
        seen: &mut BTreeSet<u64>,
        extents: &mut Vec<Extent>,
        run_count: &mut usize,
    ) -> Result<()> {
        let position = attribute
            .position()
            .value()
            .context("NTFS attribute has no disk position")?
            .get();
        if !seen.insert(position) {
            return Ok(());
        }
        ensure!(
            extents.len() < MAX_RUNS,
            "NTFS extent count exceeds the safety limit"
        );
        let h = attribute.record_data();
        ensure!(
            h.len() >= 64 && h[8] == 1,
            "invalid compressed NTFS nonresident header"
        );
        let flags = u16::from_le_bytes(h[12..14].try_into()?);
        ensure!(
            flags & 0x00ff == 1 && flags & 0x4000 == 0,
            "unsupported NTFS compression format or encrypted stream"
        );
        // Continuation records may leave the unit exponent zero; the base
        // record remains authoritative and must specify the standard 16 clusters.
        ensure!(
            h[34] == 4 || (h[34] == 0 && h[16..24] != [0; 8]),
            "unsupported NTFS compression unit exponent {}",
            h[34]
        );
        let start = u64::from_le_bytes(h[16..24].try_into()?)
            .checked_mul(cluster)
            .context("NTFS VCN overflow")?;
        let end = u64::from_le_bytes(h[24..32].try_into()?)
            .checked_add(1)
            .and_then(|v| v.checked_mul(cluster))
            .context("NTFS VCN overflow")?;
        let length = u64::from_le_bytes(h[48..56].try_into()?);
        let initialized = u64::from_le_bytes(h[56..64].try_into()?);
        let run_offset = usize::from(u16::from_le_bytes(h[32..34].try_into()?));
        ensure!(
            run_offset >= 64 && run_offset < h.len(),
            "invalid NTFS mapping-pairs offset"
        );
        let NtfsAttributeValue::NonResident(value) = attribute.value(reader)? else {
            bail!("compressed NTFS extent is not a raw nonresident attribute");
        };
        let mut runs = Vec::new();
        let mut offset = start;
        for run in value.data_runs() {
            ensure!(
                *run_count < MAX_RUNS,
                "NTFS run count exceeds the safety limit"
            );
            *run_count += 1;
            let run = run?;
            let size = run.allocated_size();
            ensure!(
                size != 0 && size % cluster == 0,
                "invalid NTFS data run length"
            );
            let next = offset
                .checked_add(size)
                .context("NTFS run length overflow")?;
            ensure!(next <= end, "NTFS data run exceeds its extent");
            let physical = run.data_position().value().map(|v| v.get());
            if let Some(physical) = physical {
                ensure!(
                    physical
                        .checked_add(size)
                        .is_some_and(|v| v <= value.ntfs().size()),
                    "NTFS data run is outside the volume"
                );
            }
            runs.push(Run {
                start: offset,
                end: next,
                physical,
            });
            offset = next;
        }
        ensure!(
            offset == end,
            "NTFS run map does not cover its declared extent"
        );
        extents.push(Extent {
            start,
            end,
            length,
            initialized,
            runs,
        });
        Ok(())
    }

    pub(crate) fn len(&self) -> u64 {
        self.length
    }

    pub(crate) fn read_exact_at<R: Read + Seek>(
        &mut self,
        reader: &mut R,
        offset: u64,
        buffer: &mut [u8],
    ) -> Result<()> {
        ensure!(
            offset
                .checked_add(buffer.len() as u64)
                .is_some_and(|end| end <= self.length),
            "read exceeds compressed NTFS file length"
        );
        let mut position = offset;
        let mut remaining = buffer;
        while !remaining.is_empty() {
            if position >= self.initialized {
                remaining.fill(0);
                break;
            }
            let unit = position / self.unit_size as u64;
            if self.cached_unit != Some(unit) {
                self.load_unit(reader, unit)
                    .with_context(|| format!("decoding NTFS compression unit {unit}"))?;
                self.cached_unit = Some(unit);
            }
            let within = (position % self.unit_size as u64) as usize;
            let count = remaining
                .len()
                .min(self.unit_size - within)
                .min((self.initialized - position) as usize);
            remaining[..count].copy_from_slice(&self.cache[within..within + count]);
            position += count as u64;
            remaining = &mut remaining[count..];
        }
        Ok(())
    }

    fn load_unit<R: Read + Seek>(&mut self, reader: &mut R, unit: u64) -> Result<()> {
        let start = unit
            .checked_mul(self.unit_size as u64)
            .context("NTFS unit offset overflow")?;
        let end = start
            .checked_add(self.unit_size as u64)
            .context("NTFS unit offset overflow")?;
        let mut position = start;
        let mut packed = Vec::with_capacity(self.unit_size);
        let mut sparse = false;
        let first = self.runs.partition_point(|run| run.end <= start);
        for run in &self.runs[first..] {
            if position == end {
                break;
            }
            ensure!(
                run.start <= position && run.end > position,
                "missing NTFS compression run"
            );
            let count = (run.end.min(end) - position) as usize;
            if let Some(physical) = run.physical {
                ensure!(
                    !sparse,
                    "allocated NTFS clusters follow sparse padding within a compression unit"
                );
                let old_len = packed.len();
                packed.resize(old_len + count, 0);
                reader.seek(SeekFrom::Start(
                    physical
                        .checked_add(position - run.start)
                        .context("NTFS physical offset overflow")?,
                ))?;
                reader.read_exact(&mut packed[old_len..])?;
            } else {
                sparse = true;
            }
            position += count as u64;
        }
        ensure!(position == end, "truncated NTFS compression unit run map");
        self.cache.fill(0);
        if packed.is_empty() {
            return Ok(());
        }
        if !sparse {
            self.cache.copy_from_slice(&packed);
            return Ok(());
        }
        let required = (self.initialized - start).min(self.unit_size as u64) as usize;
        decode_lznt1(&packed, &mut self.cache, required)
    }
}

struct Extent {
    start: u64,
    end: u64,
    length: u64,
    initialized: u64,
    runs: Vec<Run>,
}

/// Decode only complete chunks. NTFS pads the allocation after the LZNT1 end
/// marker, but initialized bytes must never be invented for a truncated stream.
fn decode_lznt1(input: &[u8], output: &mut [u8], required: usize) -> Result<()> {
    ensure!(required <= output.len(), "LZNT1 output limit exceeded");
    let mut source = 0;
    let mut destination = 0;
    while destination < required {
        let header = input
            .get(source..source + 2)
            .context("truncated LZNT1 chunk header")?;
        let header = u16::from_le_bytes(header.try_into()?);
        ensure!(header != 0, "LZNT1 stream ended before initialized data");
        ensure!(header & 0x7000 == 0x3000, "invalid LZNT1 chunk signature");
        source += 2;
        let size = usize::from(header & 0x0fff) + 1;
        let chunk = input
            .get(source..source + size)
            .context("truncated LZNT1 chunk")?;
        source += size;
        let end = (destination + CHUNK).min(output.len());
        ensure!(destination < end, "LZNT1 output limit exceeded");
        let target = &mut output[destination..end];
        let written = if header & 0x8000 == 0 {
            ensure!(
                chunk.len() <= target.len(),
                "LZNT1 raw chunk exceeds output"
            );
            target[..chunk.len()].copy_from_slice(chunk);
            chunk.len()
        } else {
            decode_chunk(chunk, target)?
        };
        ensure!(
            written == CHUNK || destination + written >= required,
            "short LZNT1 chunk before initialized data ends"
        );
        destination += written;
    }
    Ok(())
}

fn decode_chunk(input: &[u8], output: &mut [u8]) -> Result<usize> {
    let mut source = 0;
    let mut destination = 0;
    while source < input.len() {
        let flags = input[source];
        source += 1;
        ensure!(source < input.len(), "LZNT1 flag byte has no data");
        for bit in 0..8 {
            if source == input.len() {
                break;
            }
            if flags & (1 << bit) == 0 {
                ensure!(destination < output.len(), "LZNT1 literal exceeds chunk");
                output[destination] = input[source];
                destination += 1;
                source += 1;
            } else {
                let token = input
                    .get(source..source + 2)
                    .context("truncated LZNT1 back-reference")?;
                let token = u16::from_le_bytes(token.try_into()?) as usize;
                source += 2;
                let mut shift = 12;
                while shift > 4 && (1 << (16 - shift)) < destination {
                    shift -= 1;
                }
                let distance = (token >> shift) + 1;
                let length = (token & ((1 << shift) - 1)) + 3;
                ensure!(
                    distance <= destination,
                    "LZNT1 back-reference precedes chunk"
                );
                ensure!(
                    length <= output.len() - destination,
                    "LZNT1 back-reference exceeds chunk"
                );
                // Forward byte copies intentionally allow overlapping matches.
                for _ in 0..length {
                    output[destination] = output[destination - distance];
                    destination += 1;
                }
            }
        }
    }
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn repeated_chunk(byte: u8) -> Vec<u8> {
        // Literal followed by an overlapping distance-one, length-4095 match.
        vec![3, 0xb0, 2, byte, 0xfc, 0x0f]
    }

    #[test]
    fn lznt1_overlap_and_chunk_dictionary_reset() {
        let mut input = repeated_chunk(b'A');
        input.extend(repeated_chunk(b'B'));
        let mut output = vec![0; 8192];
        decode_lznt1(&input, &mut output, 8192).unwrap();
        assert_eq!(&output[..4096], &[b'A'; 4096]);
        assert_eq!(&output[4096..], &[b'B'; 4096]);
        let mut invalid = repeated_chunk(b'A');
        invalid.extend([2, 0xb0, 1, 0, 0]);
        assert!(decode_lznt1(&invalid, &mut output, 8192).is_err());
    }

    #[test]
    fn lznt1_raw_chunks_and_partial_final_chunks() {
        let mut input = vec![0xff, 0x3f];
        input.extend((0..4096).map(|i| (i % 251) as u8));
        input.extend([2, 0x30, b'e', b'n', b'd', 0, 0]);
        let mut output = vec![0; 8192];
        decode_lznt1(&input, &mut output, 4099).unwrap();
        assert_eq!(&output[..4096], &input[2..4098]);
        assert_eq!(&output[4096..4099], b"end");
        assert!(decode_lznt1(&input, &mut output, 4100).is_err());
    }

    #[test]
    fn lznt1_rejects_corrupt_and_truncated_chunks() {
        for input in [
            vec![],
            vec![0],
            vec![0, 0],
            vec![0, 0x20, 1],
            vec![3, 0xb0, 2, b'A', 0xfc],       // truncated chunk
            vec![1, 0xb0, 1, 0],                // incomplete token
            vec![2, 0xb0, 1, 0, 0],             // backward before dictionary
            vec![3, 0xb0, 2, b'A', 0xff, 0x0f], // expansion exceeds 4096
            vec![0, 0xb0, 0],                   // flag with no tokens
        ] {
            assert!(
                decode_lznt1(&input, &mut [0; 4096], 4096).is_err(),
                "{input:?}"
            );
        }
    }

    #[test]
    fn lznt1_changes_token_partition_at_exact_power_of_two() {
        for literals in [
            16usize, 17, 32, 33, 64, 65, 128, 129, 256, 257, 512, 513, 1024, 1025, 2048, 2049,
        ] {
            let expected: Vec<_> = (0..literals).map(|i| (i % 251) as u8).collect();
            let mut chunk = Vec::new();
            let mut flag_at = 0;
            for (i, &byte) in expected.iter().enumerate() {
                if i % 8 == 0 {
                    flag_at = chunk.len();
                    chunk.push(0);
                }
                chunk.push(byte);
            }
            if literals % 8 == 0 {
                flag_at = chunk.len();
                chunk.push(0);
            }
            chunk[flag_at] |= 1 << (literals % 8);
            let displacement_bits = (4..=12).find(|&bits| (1 << bits) >= literals).unwrap();
            let token = ((literals - 1) << (16 - displacement_bits)) as u16;
            chunk.extend(token.to_le_bytes());
            let mut input = (0xb000 | (chunk.len() - 1) as u16).to_le_bytes().to_vec();
            input.extend(chunk);
            let mut output = [0; 4096];
            decode_lznt1(&input, &mut output, literals + 3).unwrap();
            assert_eq!(&output[..literals], expected);
            assert_eq!(&output[literals..literals + 3], &expected[..3]);
        }
    }

    fn unit_stream() -> (CompressedStream, Cursor<Vec<u8>>, Vec<u8>) {
        let mut disk = vec![0; 24576];
        let mut compressed = repeated_chunk(b'A');
        compressed.extend(repeated_chunk(b'B'));
        disk[512..512 + compressed.len()].copy_from_slice(&compressed);
        disk[8192..16384].fill(b'C');
        let expected = [
            vec![b'A'; 4096],
            vec![b'B'; 4096],
            vec![b'C'; 8192],
            vec![0; 8192],
            vec![0; 17],
        ]
        .concat();
        let stream = CompressedStream {
            runs: vec![
                Run {
                    start: 0,
                    end: 512,
                    physical: Some(512),
                },
                Run {
                    start: 512,
                    end: 8192,
                    physical: None,
                },
                Run {
                    start: 8192,
                    end: 12288,
                    physical: Some(8192),
                },
                Run {
                    start: 12288,
                    end: 16384,
                    physical: Some(12288),
                },
                Run {
                    start: 16384,
                    end: 24576,
                    physical: None,
                },
            ],
            length: expected.len() as u64,
            initialized: 24576,
            unit_size: 8192,
            cache: vec![0; 8192],
            cached_unit: None,
        };
        (stream, Cursor::new(disk), expected)
    }

    #[test]
    fn unit_reads_cover_compressed_verbatim_sparse_and_uninitialized_ranges() {
        let (mut stream, mut reader, expected) = unit_stream();
        for (offset, size) in [
            (0, expected.len()),
            (4000, 5000),
            (8191, 2),
            (16383, 3),
            (24570, 23),
            (0, 17),
            (24593, 0),
        ] {
            let mut actual = vec![0xff; size];
            stream
                .read_exact_at(&mut reader, offset as u64, &mut actual)
                .unwrap();
            assert_eq!(actual, expected[offset..offset + size]);
        }
        assert!(
            stream
                .read_exact_at(&mut reader, u64::MAX, &mut [0; 2])
                .is_err()
        );
        assert!(
            stream
                .read_exact_at(&mut reader, 24593, &mut [0; 1])
                .is_err()
        );
        stream.initialized = 4097;
        stream.cached_unit = None;
        let mut actual = vec![0xff; expected.len()];
        stream.read_exact_at(&mut reader, 0, &mut actual).unwrap();
        assert_eq!(&actual[..4097], &expected[..4097]);
        assert!(actual[4097..].iter().all(|&byte| byte == 0));
    }

    #[test]
    fn unit_rejects_missing_runs_and_allocated_clusters_after_sparse_padding() {
        let (mut stream, mut reader, _) = unit_stream();
        stream.runs[1].end = 4096;
        assert!(stream.read_exact_at(&mut reader, 0, &mut [0; 1]).is_err());
        stream.runs.insert(
            2,
            Run {
                start: 4096,
                end: 8192,
                physical: Some(16384),
            },
        );
        assert!(stream.read_exact_at(&mut reader, 0, &mut [0; 1]).is_err());
    }
}
