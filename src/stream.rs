use serde::Serialize;
use std::io::{self, Read};
use thiserror::Error;

pub const RECORD_SIZE: usize = 312;
const CHECKSUM_OFFSET: usize = 280;
const MAX_PAYLOAD_SIZE: u64 = 256 * 1024 * 1024;
const DMU_BACKUP_MAGIC: u64 = 0x0002_f5ba_cbac;
const FEATURE_RAW: u64 = 1 << 24;

pub const DRR_BEGIN: u32 = 0;
pub const DRR_OBJECT: u32 = 1;
pub const DRR_FREEOBJECTS: u32 = 2;
pub const DRR_WRITE: u32 = 3;
pub const DRR_FREE: u32 = 4;
pub const DRR_END: u32 = 5;
pub const DRR_WRITE_BYREF: u32 = 6;
pub const DRR_SPILL: u32 = 7;
pub const DRR_WRITE_EMBEDDED: u32 = 8;
pub const DRR_OBJECT_RANGE: u32 = 9;
pub const DRR_REDACT: u32 = 10;
const DRR_NUMTYPES: u32 = 11;

#[derive(Debug, Error)]
pub enum StreamError {
    #[error("I/O error at stream offset {offset}: {source}")]
    Io {
        offset: u64,
        #[source]
        source: io::Error,
    },
    #[error("invalid ZFS send stream at offset {offset}: {message}")]
    Invalid { offset: u64, message: String },
    #[error("unsupported ZFS send stream at offset {offset}: {message}")]
    Unsupported { offset: u64, message: String },
    #[error("Fletcher-4 checksum mismatch at stream offset {offset}")]
    Checksum { offset: u64 },
}

pub type Result<T> = std::result::Result<T, StreamError>;

#[derive(Debug, Clone, Serialize)]
pub struct BeginRecord {
    pub to_guid: u64,
    pub from_guid: u64,
    pub creation_time: u64,
    pub objset_type: u32,
    pub flags: u32,
    pub features: u64,
    pub dataset_name: String,
}

#[derive(Debug, Clone, Copy)]
pub struct ObjectRecord {
    pub object: u64,
    pub object_type: u32,
    pub bonus_type: u32,
    pub block_size: u32,
    pub bonus_length: u32,
    pub max_block_id: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct WriteRecord {
    pub object: u64,
    pub object_type: u32,
    pub offset: u64,
    pub logical_size: u64,
    pub compressed_size: u64,
    pub compression_type: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct FreeRecord {
    pub object: u64,
    pub offset: u64,
    pub length: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct FreeObjectsRecord {
    pub first_object: u64,
    pub object_count: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct EmbeddedWriteRecord {
    pub object: u64,
    pub offset: u64,
    pub length: u64,
    pub compression_type: u8,
    pub logical_size: u32,
}

#[derive(Debug, Clone)]
pub enum RecordKind {
    Begin(BeginRecord),
    Object(ObjectRecord),
    FreeObjects(FreeObjectsRecord),
    Write(WriteRecord),
    Free(FreeRecord),
    End,
    WriteByRef,
    Spill,
    WriteEmbedded(EmbeddedWriteRecord),
    ObjectRange,
    Redact,
}

impl RecordKind {
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Begin(_) => "BEGIN",
            Self::Object(_) => "OBJECT",
            Self::FreeObjects(_) => "FREEOBJECTS",
            Self::Write(_) => "WRITE",
            Self::Free(_) => "FREE",
            Self::End => "END",
            Self::WriteByRef => "WRITE_BYREF",
            Self::Spill => "SPILL",
            Self::WriteEmbedded(_) => "WRITE_EMBEDDED",
            Self::ObjectRange => "OBJECT_RANGE",
            Self::Redact => "REDACT",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Record {
    pub stream_offset: u64,
    pub kind: RecordKind,
    pub payload: Vec<u8>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Fletcher4([u64; 4]);

impl Fletcher4 {
    fn reset(&mut self) {
        self.0 = [0; 4];
    }

    fn update(&mut self, bytes: &[u8], offset: u64) -> Result<()> {
        if !bytes.len().is_multiple_of(4) {
            return Err(StreamError::Invalid {
                offset,
                message: "checksummed region is not aligned to four bytes".into(),
            });
        }
        for word in bytes.chunks_exact(4) {
            let value = u32::from_le_bytes(word.try_into().expect("four-byte chunk")) as u64;
            self.0[0] = self.0[0].wrapping_add(value);
            self.0[1] = self.0[1].wrapping_add(self.0[0]);
            self.0[2] = self.0[2].wrapping_add(self.0[1]);
            self.0[3] = self.0[3].wrapping_add(self.0[2]);
        }
        Ok(())
    }
}

pub struct StreamReader<R> {
    inner: R,
    offset: u64,
    started: bool,
    ended: bool,
    checksum: Fletcher4,
    begin: Option<BeginRecord>,
}

impl<R: Read> StreamReader<R> {
    #[must_use]
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            offset: 0,
            started: false,
            ended: false,
            checksum: Fletcher4::default(),
            begin: None,
        }
    }

    #[must_use]
    pub fn begin(&self) -> Option<&BeginRecord> {
        self.begin.as_ref()
    }

    #[must_use]
    pub fn saw_end(&self) -> bool {
        self.ended
    }

    pub fn next_record(&mut self) -> Result<Option<Record>> {
        let record_offset = self.offset;
        let mut header = [0_u8; RECORD_SIZE];
        match self.inner.read(&mut header[..1]) {
            Ok(0) if self.ended => return Ok(None),
            Ok(0) if !self.started => return Ok(None),
            Ok(0) => {
                return Err(StreamError::Invalid {
                    offset: record_offset,
                    message: "stream ended before an END record".into(),
                });
            }
            Ok(_) => {}
            Err(source) => {
                return Err(StreamError::Io {
                    offset: record_offset,
                    source,
                });
            }
        }
        self.inner
            .read_exact(&mut header[1..])
            .map_err(|source| StreamError::Io {
                offset: record_offset,
                source,
            })?;

        if self.ended {
            return Err(StreamError::Invalid {
                offset: record_offset,
                message: "trailing data follows the END record".into(),
            });
        }

        let record_type = le_u32(&header, 0);
        if record_type >= DRR_NUMTYPES {
            return Err(StreamError::Invalid {
                offset: record_offset,
                message: format!("unknown replay record type {record_type}"),
            });
        }
        if !self.started && record_type != DRR_BEGIN {
            return Err(StreamError::Invalid {
                offset: record_offset,
                message: "first replay record is not BEGIN".into(),
            });
        }
        if self.started && record_type == DRR_BEGIN {
            return Err(StreamError::Unsupported {
                offset: record_offset,
                message: "compound or concatenated streams are not supported".into(),
            });
        }

        let payload_size = payload_size(&header, record_type, record_offset)?;
        if payload_size > MAX_PAYLOAD_SIZE {
            return Err(StreamError::Invalid {
                offset: record_offset,
                message: format!("record payload is too large ({payload_size} bytes)"),
            });
        }
        let payload_len = usize::try_from(payload_size).map_err(|_| StreamError::Invalid {
            offset: record_offset,
            message: "record payload does not fit in memory".into(),
        })?;
        let mut payload = vec![0_u8; payload_len];
        self.inner
            .read_exact(&mut payload)
            .map_err(|source| StreamError::Io {
                offset: record_offset + RECORD_SIZE as u64,
                source,
            })?;

        let kind = decode_kind(&header, record_type, record_offset)?;
        self.validate_checksum(&header, &payload, record_type, record_offset)?;

        if let RecordKind::Begin(begin) = &kind {
            if le_u64(&header, 8) != DMU_BACKUP_MAGIC {
                let swapped = u64::from_be_bytes(header[8..16].try_into().expect("eight bytes"));
                let message = if swapped == DMU_BACKUP_MAGIC {
                    "big-endian streams are not supported in this first release"
                } else {
                    "bad ZFS send magic number"
                };
                return Err(StreamError::Unsupported {
                    offset: record_offset,
                    message: message.into(),
                });
            }
            if begin.features & FEATURE_RAW != 0 {
                return Err(StreamError::Unsupported {
                    offset: record_offset,
                    message: "raw/encrypted send streams are not supported".into(),
                });
            }
            self.begin = Some(begin.clone());
            self.started = true;
        }
        if matches!(kind, RecordKind::End) {
            self.ended = true;
        }

        self.offset = self
            .offset
            .checked_add(RECORD_SIZE as u64 + payload_size)
            .ok_or_else(|| StreamError::Invalid {
                offset: record_offset,
                message: "stream offset overflow".into(),
            })?;

        Ok(Some(Record {
            stream_offset: record_offset,
            kind,
            payload,
        }))
    }

    fn validate_checksum(
        &mut self,
        header: &[u8; RECORD_SIZE],
        payload: &[u8],
        record_type: u32,
        offset: u64,
    ) -> Result<()> {
        if record_type == DRR_BEGIN {
            self.checksum.reset();
            self.checksum.update(header, offset)?;
            self.checksum.update(payload, offset + RECORD_SIZE as u64)?;
            return Ok(());
        }

        if record_type == DRR_END {
            let expected = read_checksum(header, 8);
            if expected != [0; 4] && expected != self.checksum.0 {
                return Err(StreamError::Checksum { offset: offset + 8 });
            }
        }

        self.checksum.update(&header[..CHECKSUM_OFFSET], offset)?;
        let record_checksum = read_checksum(header, CHECKSUM_OFFSET);
        if record_checksum != [0; 4] && record_checksum != self.checksum.0 {
            return Err(StreamError::Checksum {
                offset: offset + CHECKSUM_OFFSET as u64,
            });
        }

        if record_type != DRR_END {
            self.checksum
                .update(&header[CHECKSUM_OFFSET..], offset + CHECKSUM_OFFSET as u64)?;
            self.checksum.update(payload, offset + RECORD_SIZE as u64)?;
        }
        Ok(())
    }
}

fn decode_kind(header: &[u8; RECORD_SIZE], record_type: u32, offset: u64) -> Result<RecordKind> {
    Ok(match record_type {
        DRR_BEGIN => {
            let magic = le_u64(header, 8);
            if magic != DMU_BACKUP_MAGIC
                && u64::from_be_bytes(header[8..16].try_into().expect("eight bytes"))
                    != DMU_BACKUP_MAGIC
            {
                return Err(StreamError::Invalid {
                    offset,
                    message: format!("bad ZFS send magic 0x{magic:016x}"),
                });
            }
            let version_info = le_u64(header, 16);
            let header_type = version_info & 0b11;
            if header_type != 1 {
                return Err(StreamError::Unsupported {
                    offset,
                    message: format!("stream header type {header_type} is not a single substream"),
                });
            }
            let name_bytes = &header[56..312];
            let name_end = name_bytes
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(name_bytes.len());
            RecordKind::Begin(BeginRecord {
                to_guid: le_u64(header, 40),
                from_guid: le_u64(header, 48),
                creation_time: le_u64(header, 24),
                objset_type: le_u32(header, 32),
                flags: le_u32(header, 36),
                features: (version_info >> 2) & ((1_u64 << 56) - 1),
                dataset_name: String::from_utf8_lossy(&name_bytes[..name_end]).into_owned(),
            })
        }
        DRR_OBJECT => RecordKind::Object(ObjectRecord {
            object: le_u64(header, 8),
            object_type: le_u32(header, 16),
            bonus_type: le_u32(header, 20),
            block_size: le_u32(header, 24),
            bonus_length: le_u32(header, 28),
            max_block_id: le_u64(header, 64),
        }),
        DRR_FREEOBJECTS => RecordKind::FreeObjects(FreeObjectsRecord {
            first_object: le_u64(header, 8),
            object_count: le_u64(header, 16),
        }),
        DRR_WRITE => RecordKind::Write(WriteRecord {
            object: le_u64(header, 8),
            object_type: le_u32(header, 16),
            offset: le_u64(header, 24),
            logical_size: le_u64(header, 32),
            compressed_size: le_u64(header, 96),
            compression_type: header[50],
        }),
        DRR_FREE => RecordKind::Free(FreeRecord {
            object: le_u64(header, 8),
            offset: le_u64(header, 16),
            length: le_u64(header, 24),
        }),
        DRR_END => RecordKind::End,
        DRR_WRITE_BYREF => RecordKind::WriteByRef,
        DRR_SPILL => RecordKind::Spill,
        DRR_WRITE_EMBEDDED => RecordKind::WriteEmbedded(EmbeddedWriteRecord {
            object: le_u64(header, 8),
            offset: le_u64(header, 16),
            length: le_u64(header, 24),
            compression_type: header[40],
            logical_size: le_u32(header, 48),
        }),
        DRR_OBJECT_RANGE => RecordKind::ObjectRange,
        DRR_REDACT => RecordKind::Redact,
        _ => unreachable!("record type was range checked"),
    })
}

fn payload_size(header: &[u8; RECORD_SIZE], record_type: u32, offset: u64) -> Result<u64> {
    let size = match record_type {
        DRR_BEGIN => u64::from(le_u32(header, 4)),
        DRR_OBJECT => {
            let raw = le_u32(header, 36);
            if raw != 0 {
                u64::from(raw)
            } else {
                round_up_8(u64::from(le_u32(header, 28)), offset)?
            }
        }
        DRR_WRITE => {
            if header[50] == 0 {
                le_u64(header, 32)
            } else {
                le_u64(header, 96)
            }
        }
        DRR_SPILL => {
            let compressed = le_u64(header, 40);
            if compressed == 0 {
                le_u64(header, 16)
            } else {
                compressed
            }
        }
        DRR_WRITE_EMBEDDED => round_up_8(u64::from(le_u32(header, 52)), offset)?,
        _ => 0,
    };
    Ok(size)
}

fn round_up_8(value: u64, offset: u64) -> Result<u64> {
    value
        .checked_add(7)
        .map(|v| v & !7)
        .ok_or_else(|| StreamError::Invalid {
            offset,
            message: "payload length overflow".into(),
        })
}

fn le_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("checked record field"),
    )
}

fn le_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .expect("checked record field"),
    )
}

fn read_checksum(bytes: &[u8], offset: usize) -> [u64; 4] {
    [
        le_u64(bytes, offset),
        le_u64(bytes, offset + 8),
        le_u64(bytes, offset + 16),
        le_u64(bytes, offset + 24),
    ]
}

#[cfg(test)]
mod tests {
    use super::{DMU_BACKUP_MAGIC, Fletcher4, RECORD_SIZE};

    #[test]
    fn fletcher_4_matches_a_hand_calculated_sequence() {
        let mut checksum = Fletcher4::default();
        checksum.update(&1_u32.to_le_bytes(), 0).unwrap();
        checksum.update(&2_u32.to_le_bytes(), 4).unwrap();
        assert_eq!(checksum.0, [3, 4, 5, 6]);
    }

    #[test]
    fn constants_match_the_wire_format() {
        assert_eq!(RECORD_SIZE, 312);
        assert_eq!(DMU_BACKUP_MAGIC, 0x0002_f5ba_cbac);
    }
}
