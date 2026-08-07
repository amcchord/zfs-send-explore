//! OpenZFS on-disk block decompression shared by pool-member and raw-send reads.

use anyhow::{Context, Result, bail};
use flate2::read::ZlibDecoder;
use std::io::Read;
use zfs_core::{CompressType, compress};

const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

pub(crate) fn decompress_block(
    compression: u8,
    payload: &[u8],
    logical_size: u64,
) -> Result<Vec<u8>> {
    let logical_size = usize::try_from(logical_size).context("logical block size is too large")?;
    let kind = CompressType::from_raw(compression);
    match kind {
        CompressType::Inherit | CompressType::On => {
            bail!("compression sentinel {compression} is invalid in an on-disk block pointer")
        }
        CompressType::Off => {
            if payload.len() != logical_size {
                bail!(
                    "uncompressed ZFS block is {} bytes, expected {logical_size}",
                    payload.len()
                );
            }
            Ok(payload.to_vec())
        }
        CompressType::Gzip(_) => decompress_gzip(payload, logical_size),
        CompressType::Zstd => decompress_zstd(payload, logical_size),
        _ => compress::decompress(kind, payload, logical_size).map_err(Into::into),
    }
}

/// Decode a non-raw DRR_WRITE payload. A zero wire value means that the
/// sender placed the complete logical block in the stream; non-zero values
/// are OpenZFS compression identifiers from a `zfs send -c` stream.
pub(crate) fn decode_replay_write(
    compression: u8,
    payload: &[u8],
    logical_size: u64,
) -> Result<Vec<u8>> {
    if compression == 0 {
        let logical_size =
            usize::try_from(logical_size).context("logical replay WRITE size is too large")?;
        if payload.len() != logical_size {
            bail!(
                "uncompressed replay WRITE is {} bytes, expected {logical_size}",
                payload.len()
            );
        }
        return Ok(payload.to_vec());
    }
    decompress_block(compression, payload, logical_size)
}

/// Decode the meaningful bytes of a DRR_WRITE_EMBEDDED payload. The replay
/// payload is padded to an eight-byte boundary, while `physical_size` excludes
/// that padding and `logical_size` is the size after decompression.
pub(crate) fn decode_embedded_write(
    compression: u8,
    embedded_type: u8,
    payload: &[u8],
    physical_size: u32,
    logical_size: u32,
) -> Result<Vec<u8>> {
    if embedded_type != 0 {
        bail!("unsupported ZFS embedded block type {embedded_type}");
    }
    let physical_size =
        usize::try_from(physical_size).context("embedded replay payload size is too large")?;
    let encoded = payload
        .get(..physical_size)
        .context("embedded replay payload is shorter than its physical size")?;
    decompress_block(compression, encoded, u64::from(logical_size))
}

fn decompress_gzip(payload: &[u8], logical_size: usize) -> Result<Vec<u8>> {
    let limit = logical_size
        .checked_add(1)
        .context("logical gzip block size overflow")?;
    let mut decoder = ZlibDecoder::new(payload).take(limit as u64);
    let mut output = Vec::with_capacity(logical_size);
    decoder
        .read_to_end(&mut output)
        .context("decompressing ZFS gzip block")?;
    if output.len() != logical_size {
        bail!(
            "ZFS gzip block expanded to {} bytes, expected {logical_size}",
            output.len()
        );
    }
    Ok(output)
}

fn decompress_zstd(payload: &[u8], logical_size: usize) -> Result<Vec<u8>> {
    let header = payload
        .get(..8)
        .context("ZFS Zstandard block is missing its 8-byte header")?;
    let compressed_size =
        u32::from_be_bytes(header[..4].try_into().expect("four-byte checked range")) as usize;
    let end = 8usize
        .checked_add(compressed_size)
        .context("ZFS Zstandard compressed-size overflow")?;
    let frame = payload
        .get(8..end)
        .context("ZFS Zstandard block has an invalid compressed-size header")?;

    // OpenZFS uses ZSTD_f_zstd1_magicless. Reintroducing the standard magic
    // lets the pure-Rust decoder consume the otherwise standard frame while
    // the c_len field above excludes sector-alignment padding.
    let mut framed = Vec::with_capacity(ZSTD_MAGIC.len() + frame.len());
    framed.extend_from_slice(&ZSTD_MAGIC);
    framed.extend_from_slice(frame);
    let limit = logical_size
        .checked_add(1)
        .context("logical Zstandard block size overflow")?;
    let mut decoder = ruzstd::decoding::StreamingDecoder::new(framed.as_slice())
        .context("decoding the ZFS Zstandard frame header")?
        .take(limit as u64);
    let mut output = Vec::with_capacity(logical_size);
    decoder
        .read_to_end(&mut output)
        .context("decompressing ZFS Zstandard block")?;
    if output.len() != logical_size {
        bail!(
            "ZFS Zstandard block expanded to {} bytes, expected {logical_size}",
            output.len()
        );
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::{ZSTD_MAGIC, decompress_block};
    use flate2::{Compression, write::ZlibEncoder};
    use ruzstd::encoding::{CompressionLevel, compress_to_vec};
    use std::io::Write;
    use zfs_core::CompressType;

    #[test]
    fn dispatches_every_supported_zfs_codec() {
        assert_eq!(
            decompress_block(CompressType::Off.raw(), b"plain", 5).unwrap(),
            b"plain"
        );
        assert_eq!(
            decompress_block(CompressType::Empty.raw(), &[], 5).unwrap(),
            [0; 5]
        );
        assert_eq!(
            decompress_block(CompressType::Lzjb.raw(), &[0, b'A', b'B', b'C', b'D'], 4).unwrap(),
            b"ABCD"
        );
        assert_eq!(
            decompress_block(CompressType::Zle.raw(), &[0x02, 7, 8, 9], 3).unwrap(),
            [7, 8, 9]
        );

        let plain = b"compressed pool member".repeat(64);
        let lz4 = lz4_flex::block::compress(&plain);
        let mut zfs_lz4 = Vec::with_capacity(4 + lz4.len());
        zfs_lz4.extend_from_slice(&(lz4.len() as u32).to_be_bytes());
        zfs_lz4.extend_from_slice(&lz4);
        assert_eq!(
            decompress_block(CompressType::Lz4.raw(), &zfs_lz4, plain.len() as u64).unwrap(),
            plain
        );

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&plain).unwrap();
        let gzip = encoder.finish().unwrap();
        assert_eq!(
            decompress_block(CompressType::Gzip(6).raw(), &gzip, plain.len() as u64).unwrap(),
            plain
        );
    }

    #[test]
    fn decodes_the_magicless_openzfs_zstandard_envelope() {
        let plain = b"real OpenZFS uses a magicless Zstandard frame".repeat(128);
        let standard = compress_to_vec(plain.as_slice(), CompressionLevel::Fastest);
        assert_eq!(standard.get(..4), Some(ZSTD_MAGIC.as_slice()));
        let magicless = &standard[4..];
        let mut zfs = Vec::with_capacity(8 + magicless.len() + 64);
        zfs.extend_from_slice(&(magicless.len() as u32).to_be_bytes());
        // The level/version word is authenticated by the block checksum but is
        // not needed to decompress the self-describing frame.
        zfs.extend_from_slice(&[3, 0, 0x28, 0xa5]);
        zfs.extend_from_slice(magicless);
        zfs.extend_from_slice(&[0; 64]);
        assert_eq!(
            decompress_block(CompressType::Zstd.raw(), &zfs, plain.len() as u64).unwrap(),
            plain
        );

        zfs[0..4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(decompress_block(CompressType::Zstd.raw(), &zfs, plain.len() as u64).is_err());
    }

    #[test]
    fn rejects_on_disk_compression_sentinels() {
        for sentinel in [CompressType::Inherit.raw(), CompressType::On.raw()] {
            assert!(decompress_block(sentinel, &[], 1).is_err());
        }
    }
}
