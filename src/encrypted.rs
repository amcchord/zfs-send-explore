use aes::{Aes128, Aes192, Aes256};
use aes_gcm::{
    Aes128Gcm, Aes256Gcm, AesGcm, KeyInit as _,
    aead::{AeadInPlace, generic_array::GenericArray},
};
use anyhow::{Context, Result, anyhow, bail};
use ccm::{
    Ccm,
    consts::{U12, U16},
};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use pbkdf2::pbkdf2_hmac;
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha512};
use std::collections::{BTreeMap, BTreeSet};
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

const KEYFORMAT_RAW: u64 = 1;
const KEYFORMAT_HEX: u64 = 2;
const KEYFORMAT_PASSPHRASE: u64 = 3;
const MAX_PBKDF2_ITERATIONS: u64 = 10_000_000;

#[derive(Debug, Clone)]
pub struct EncryptionParams {
    pub suite: u64,
    pub guid: u64,
    pub version: u64,
    pub key_format: u64,
    pub pbkdf2_iterations: u64,
    pub pbkdf2_salt: u64,
    wrapped_master_key: Vec<u8>,
    wrapped_hmac_key: Vec<u8>,
    wrapping_iv: [u8; 12],
    wrapping_mac: [u8; 16],
}

#[derive(Debug, Zeroize, ZeroizeOnDrop)]
pub struct DatasetKey {
    #[zeroize(skip)]
    suite: u64,
    master_key: Vec<u8>,
    hmac_key: [u8; 64],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawBlockPointer {
    pub block_id: u64,
    pub object_type: u32,
    pub logical_size: u64,
    pub physical_size: u64,
    pub compression: u8,
    pub flags: u8,
    pub mac: [u8; 16],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawDnode {
    pub object: u64,
    pub object_type: u32,
    pub bonus_type: u32,
    pub block_size: u32,
    pub bonus_length: u32,
    pub checksum_type: u8,
    pub compression: u8,
    pub slots: u8,
    pub flags: u8,
    pub indirect_block_shift: u8,
    pub levels: u8,
    pub block_pointers: u8,
    pub max_block_id: u64,
    pub bonus_ciphertext: Vec<u8>,
    pub blocks: Vec<RawBlockPointer>,
    pub spill: Option<RawBlockPointer>,
}

/// Portable raw-send state needed to authenticate the next incremental
/// OBJECT_RANGE containing an extracted file. No key material is stored here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawSidecarState {
    pub crypto_guid: u64,
    pub crypto_version: u64,
    pub first_object: u64,
    pub object_slots: u64,
    pub dnodes: Vec<RawDnode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_spill: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy)]
pub struct RawDnodeRange {
    pub first_object: u64,
    pub object_slots: u64,
    pub salt: [u8; 8],
    pub iv: [u8; 12],
    pub mac: [u8; 16],
    pub flags: u8,
    pub crypto_version: u64,
}

#[derive(Debug)]
enum NvValue {
    Uint64(u64),
    Bytes(Vec<u8>),
    List(BTreeMap<String, NvValue>),
    Ignored,
}

impl EncryptionParams {
    pub fn from_begin_payload(payload: &[u8]) -> Result<Self> {
        let values = NvDecoder::new(payload).decode()?;
        let crypto = match values.get("crypt_keydata") {
            Some(NvValue::List(value)) => value,
            _ => bail!("raw stream BEGIN record has no crypt_keydata nvlist"),
        };

        let wrapping_iv = array::<12>(bytes(crypto, "DSL_CRYPTO_IV")?, "DSL_CRYPTO_IV")?;
        let wrapping_mac = array::<16>(bytes(crypto, "DSL_CRYPTO_MAC")?, "DSL_CRYPTO_MAC")?;
        Ok(Self {
            suite: uint64(crypto, "DSL_CRYPTO_SUITE")?,
            guid: uint64(crypto, "DSL_CRYPTO_GUID")?,
            version: uint64(crypto, "DSL_CRYPTO_VERSION")?,
            key_format: uint64(crypto, "keyformat")?,
            pbkdf2_iterations: uint64(crypto, "pbkdf2iters")?,
            pbkdf2_salt: uint64(crypto, "pbkdf2salt")?,
            wrapped_master_key: bytes(crypto, "DSL_CRYPTO_MASTER_KEY_1")?.to_vec(),
            wrapped_hmac_key: bytes(crypto, "DSL_CRYPTO_HMAC_KEY_1")?.to_vec(),
            wrapping_iv,
            wrapping_mac,
        })
    }

    pub fn key_format_name(&self) -> Result<&'static str> {
        match self.key_format {
            KEYFORMAT_RAW => Ok("raw"),
            KEYFORMAT_HEX => Ok("hex"),
            KEYFORMAT_PASSPHRASE => Ok("passphrase"),
            value => bail!("unsupported ZFS keyformat value {value}"),
        }
    }

    pub fn unlock(&self, material: &[u8]) -> Result<DatasetKey> {
        let mut wrapping_key = Zeroizing::new([0_u8; 32]);
        match self.key_format {
            KEYFORMAT_RAW => {
                if material.len() != wrapping_key.len() {
                    bail!("raw ZFS key must be exactly 32 bytes");
                }
                wrapping_key.copy_from_slice(material);
            }
            KEYFORMAT_HEX => decode_hex_key(material, &mut wrapping_key)?,
            KEYFORMAT_PASSPHRASE => {
                if !(8..=512).contains(&material.len()) {
                    bail!("ZFS passphrase must contain between 8 and 512 bytes");
                }
                if self.pbkdf2_iterations > MAX_PBKDF2_ITERATIONS {
                    bail!(
                        "PBKDF2 iteration count {} exceeds the safety limit of {MAX_PBKDF2_ITERATIONS}",
                        self.pbkdf2_iterations
                    );
                }
                let iterations = u32::try_from(self.pbkdf2_iterations)
                    .context("PBKDF2 iteration count is too large")?;
                if iterations == 0 {
                    bail!("raw stream has an invalid zero PBKDF2 iteration count");
                }
                if material.contains(&0) {
                    bail!("passphrase contains a NUL byte, which OpenZFS does not support");
                }
                pbkdf2_hmac::<Sha1>(
                    material,
                    &self.pbkdf2_salt.to_le_bytes(),
                    iterations,
                    &mut wrapping_key[..],
                );
            }
            value => bail!("unsupported ZFS keyformat value {value}"),
        }

        let key_len = suite_key_len(self.suite)?;
        if self.wrapped_master_key.len() != key_len || self.wrapped_hmac_key.len() != 64 {
            bail!("raw stream contains malformed wrapped key material");
        }
        let mut ciphertext = Vec::with_capacity(key_len + 64);
        ciphertext.extend_from_slice(&self.wrapped_master_key);
        ciphertext.extend_from_slice(&self.wrapped_hmac_key);
        let mut aad = Vec::with_capacity(24);
        aad.extend_from_slice(&self.guid.to_le_bytes());
        if self.version == 0 {
            // The original encryption format authenticated only the key GUID.
        } else if self.version == 1 {
            aad.extend_from_slice(&self.suite.to_le_bytes());
            aad.extend_from_slice(&self.version.to_le_bytes());
        } else {
            bail!("unsupported ZFS encryption key version {}", self.version);
        }

        decrypt_aead(
            self.suite,
            &wrapping_key[..],
            &self.wrapping_iv,
            &aad,
            &mut ciphertext,
            &self.wrapping_mac,
        )
        .map_err(|_| anyhow!("the supplied key did not authenticate the encrypted ZFS dataset"))?;
        let mut hmac_key = [0_u8; 64];
        hmac_key.copy_from_slice(&ciphertext[key_len..]);
        let master_key = ciphertext[..key_len].to_vec();
        ciphertext.zeroize();
        Ok(DatasetKey {
            suite: self.suite,
            master_key,
            hmac_key,
        })
    }
}

impl DatasetKey {
    pub fn decrypt_block(
        &self,
        salt: &[u8; 8],
        iv: &[u8; 12],
        mac: &[u8; 16],
        aad: &[u8],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>> {
        let mut key = Zeroizing::new(vec![0_u8; self.master_key.len()]);
        Hkdf::<Sha512>::new(None, &self.master_key)
            .expand(salt, &mut key)
            .map_err(|_| anyhow!("could not derive an encrypted ZFS block key"))?;
        let mut plaintext = ciphertext.to_vec();
        decrypt_aead(self.suite, &key, iv, aad, &mut plaintext, mac)
            .map_err(|_| anyhow!("encrypted ZFS block failed authentication"))?;
        Ok(plaintext)
    }

    pub fn authenticate_block(&self, data: &[u8], expected: &[u8; 16]) -> Result<()> {
        let mut hmac = <Hmac<Sha512> as Mac>::new_from_slice(&self.hmac_key)
            .expect("SHA-512 HMAC accepts a 64-byte key");
        hmac.update(data);
        let digest = hmac.finalize().into_bytes();
        if !bool::from(digest[..expected.len()].ct_eq(expected)) {
            bail!("authenticated ZFS block failed HMAC verification");
        }
        Ok(())
    }

    pub fn decrypt_dnode_bonuses(
        &self,
        range: RawDnodeRange,
        dnodes: &[RawDnode],
    ) -> Result<BTreeMap<u64, Vec<u8>>> {
        if range.flags & 0x02 != 0 {
            bail!("byteswapped raw dnode blocks are unsupported");
        }
        if range.object_slots != 32 {
            bail!(
                "raw OBJECT_RANGE has {} slots; OpenZFS dnode blocks require 32",
                range.object_slots
            );
        }
        let range_end = range
            .first_object
            .checked_add(range.object_slots)
            .ok_or_else(|| anyhow!("raw OBJECT_RANGE overflows its object IDs"))?;
        let by_object = dnodes
            .iter()
            .map(|dnode| (dnode.object, dnode))
            .collect::<BTreeMap<_, _>>();
        if by_object.len() != dnodes.len() {
            bail!("raw OBJECT_RANGE contains duplicate OBJECT records");
        }
        let mut aad = Vec::new();
        let mut ciphertext = Vec::new();
        let mut encrypted_lengths = Vec::new();
        let mut visited = BTreeSet::new();
        let mut object = range.first_object;

        while object < range_end {
            let Some(dnode) = by_object.get(&object).copied() else {
                // A free single-slot dnode is all zero. The entire slot is AAD.
                aad.resize(aad.len() + 512, 0);
                object += 1;
                continue;
            };
            visited.insert(dnode.object);
            validate_dnode(range, dnode, range_end)?;
            aad.extend_from_slice(&dnode_core(dnode)?);
            for pointer in root_block_pointers(dnode, range.crypto_version)? {
                aad.extend_from_slice(&pointer.encode(range.crypto_version)?);
            }
            if dnode.flags & 0x04 != 0 {
                let spill = dnode.spill.as_ref().ok_or_else(|| {
                    anyhow!(
                        "raw object {} is missing its spill block pointer",
                        dnode.object
                    )
                })?;
                let pointer = BlockPointerAuth {
                    prop: leaf_prop(spill, range.crypto_version)?,
                    mac: spill.mac,
                };
                aad.extend_from_slice(&pointer.encode(range.crypto_version)?);
            }

            let max_bonus = dnode_max_bonus(dnode)?;
            if is_encrypted_object_type(dnode.bonus_type) && dnode.bonus_length != 0 {
                if dnode.bonus_ciphertext.len() != max_bonus {
                    bail!(
                        "raw bonus for object {} is {} bytes, expected {max_bonus}",
                        dnode.object,
                        dnode.bonus_ciphertext.len()
                    );
                }
                ciphertext.extend_from_slice(&dnode.bonus_ciphertext);
                encrypted_lengths.push((dnode.object, max_bonus));
            } else {
                if dnode.bonus_ciphertext.len() > max_bonus {
                    bail!("raw bonus for object {} exceeds its dnode", dnode.object);
                }
                aad.extend_from_slice(&dnode.bonus_ciphertext);
                aad.resize(aad.len() + max_bonus - dnode.bonus_ciphertext.len(), 0);
            }
            object += u64::from(dnode.slots);
        }
        if visited.len() != dnodes.len() {
            bail!("raw OBJECT records overlap or fall outside their OBJECT_RANGE");
        }

        let plaintext = self
            .decrypt_block(&range.salt, &range.iv, &range.mac, &aad, &ciphertext)
            .context("authenticating raw ZFS dnode metadata")?;
        let mut cursor = 0;
        let mut bonuses = BTreeMap::new();
        for (object, length) in encrypted_lengths {
            let end = cursor + length;
            bonuses.insert(object, plaintext[cursor..end].to_vec());
            cursor = end;
        }
        if cursor != plaintext.len() {
            bail!("decrypted dnode bonus data has an inconsistent length");
        }
        Ok(bonuses)
    }
}

pub fn is_encrypted_object_type(object_type: u32) -> bool {
    if object_type & 0x80 != 0 {
        return object_type & 0x20 != 0;
    }
    matches!(
        object_type,
        9 | 10 | 18 | 19 | 20 | 22 | 23 | 25 | 26 | 33 | 34 | 35 | 39 | 40 | 44 | 45 | 46 | 47 | 49
    )
}

#[derive(Debug, Clone, Copy)]
struct BlockPointerAuth {
    prop: u64,
    mac: [u8; 16],
}

impl BlockPointerAuth {
    const HOLE: Self = Self {
        prop: 0,
        mac: [0; 16],
    };

    fn encode(self, crypto_version: u64) -> Result<Vec<u8>> {
        let mut bytes = [0_u8; 32];
        bytes[..8].copy_from_slice(&self.prop.to_le_bytes());
        bytes[8..24].copy_from_slice(&self.mac);
        let length = match crypto_version {
            0 => 24,
            1 => 32,
            value => bail!("unsupported ZFS encryption key version {value}"),
        };
        Ok(bytes[..length].to_vec())
    }
}

fn validate_dnode(range: RawDnodeRange, dnode: &RawDnode, range_end: u64) -> Result<()> {
    if dnode.object < range.first_object
        || dnode.object.checked_add(u64::from(dnode.slots)).is_none()
        || dnode.object + u64::from(dnode.slots) > range_end
    {
        bail!("object {} falls outside its raw OBJECT_RANGE", dnode.object);
    }
    if dnode.slots == 0 || dnode.block_pointers == 0 || dnode.levels == 0 {
        bail!("object {} has an invalid raw dnode layout", dnode.object);
    }
    if dnode.flags & !0x06 != 0 {
        bail!("object {} has unsupported raw dnode flags", dnode.object);
    }
    if dnode.flags & 0x02 != 0 || dnode.blocks.iter().any(|block| block.flags & 0x02 != 0) {
        bail!("byteswapped raw encrypted objects are unsupported");
    }
    if usize::try_from(dnode.bonus_length).unwrap_or(usize::MAX) > dnode_max_bonus(dnode)? {
        bail!("object {} has a bonus that exceeds its dnode", dnode.object);
    }
    Ok(())
}

fn dnode_core(dnode: &RawDnode) -> Result<[u8; 64]> {
    let object_type = u8::try_from(dnode.object_type).context("dnode object type exceeds 255")?;
    let bonus_type = u8::try_from(dnode.bonus_type).context("dnode bonus type exceeds 255")?;
    let sectors = dnode.block_size / 512;
    if sectors == 0 || !dnode.block_size.is_multiple_of(512) || sectors > u32::from(u16::MAX) {
        bail!("object {} has an invalid dnode block size", dnode.object);
    }
    let bonus_length = u16::try_from(dnode.bonus_length).context("dnode bonus is too large")?;
    let mut core = [0_u8; 64];
    core[0] = object_type;
    core[1] = dnode.indirect_block_shift;
    core[2] = dnode.levels;
    core[3] = dnode.block_pointers;
    core[4] = bonus_type;
    core[5] = dnode.checksum_type;
    core[6] = dnode.compression;
    core[7] = dnode.flags & 0x04;
    core[8..10].copy_from_slice(&(sectors as u16).to_le_bytes());
    core[10..12].copy_from_slice(&bonus_length.to_le_bytes());
    core[12] = dnode.slots - 1;
    core[16..24].copy_from_slice(&dnode.max_block_id.to_le_bytes());
    Ok(core)
}

fn dnode_max_bonus(dnode: &RawDnode) -> Result<usize> {
    let dnode_size = usize::from(dnode.slots) * 512;
    let pointers = usize::from(dnode.block_pointers) * 128;
    let spill = usize::from(dnode.flags & 0x04 != 0) * 128;
    dnode_size
        .checked_sub(64 + pointers + spill)
        .ok_or_else(|| anyhow!("object {} has an impossible bonus layout", dnode.object))
}

fn root_block_pointers(dnode: &RawDnode, crypto_version: u64) -> Result<Vec<BlockPointerAuth>> {
    if dnode.indirect_block_shift < 9 || dnode.indirect_block_shift > 24 {
        bail!(
            "object {} has an unsupported indirect block shift",
            dnode.object
        );
    }
    let epb = 1_usize << (dnode.indirect_block_shift - 7);
    let leaf_count = usize::try_from(dnode.max_block_id)
        .context("object max block ID is too large")?
        .checked_add(1)
        .ok_or_else(|| anyhow!("object block count overflow"))?;
    if leaf_count > 4 * 1024 * 1024 {
        bail!(
            "object {} has too many blocks for the current raw-send safety limit",
            dnode.object
        );
    }
    let mut level = vec![BlockPointerAuth::HOLE; leaf_count];
    for block in &dnode.blocks {
        let block_id = usize::try_from(block.block_id).context("block ID is too large")?;
        if block_id >= level.len() {
            bail!("object {} WRITE is past its max block ID", dnode.object);
        }
        if level[block_id].prop != 0 {
            bail!("object {} has duplicate raw WRITE blocks", dnode.object);
        }
        level[block_id] = BlockPointerAuth {
            prop: leaf_prop(block, crypto_version)?,
            mac: block.mac,
        };
    }

    for tree_level in 1..dnode.levels {
        let mut parents = Vec::with_capacity(level.len().div_ceil(epb));
        for children in level.chunks(epb) {
            let mut digest = Sha512::new();
            for child in children {
                digest.update(child.encode(crypto_version)?);
            }
            for _ in children.len()..epb {
                digest.update(BlockPointerAuth::HOLE.encode(crypto_version)?);
            }
            let digest = digest.finalize();
            let mut mac = [0_u8; 16];
            mac.copy_from_slice(&digest[..16]);
            parents.push(BlockPointerAuth {
                prop: indirect_prop(dnode, tree_level)?,
                mac,
            });
        }
        level = parents;
    }

    let pointer_count = usize::from(dnode.block_pointers);
    if level.len() > pointer_count {
        bail!(
            "object {} needs {} root block pointers but its dnode has {pointer_count}",
            dnode.object,
            level.len()
        );
    }
    level.resize(pointer_count, BlockPointerAuth::HOLE);
    Ok(level)
}

fn leaf_prop(block: &RawBlockPointer, crypto_version: u64) -> Result<u64> {
    block_prop(
        block.logical_size,
        if crypto_version == 0 {
            512
        } else {
            block.physical_size
        },
        block.compression,
        block.object_type,
        0,
        1,
    )
}

fn indirect_prop(dnode: &RawDnode, level: u8) -> Result<u64> {
    block_prop(
        1_u64 << dnode.indirect_block_shift,
        512,
        0,
        dnode.object_type,
        level,
        0,
    )
}

fn block_prop(
    logical_size: u64,
    physical_size: u64,
    compression: u8,
    object_type: u32,
    level: u8,
    byteorder: u64,
) -> Result<u64> {
    if compression > 0x7f || level > 0x1f || byteorder > 1 {
        bail!("raw block has invalid block-pointer properties");
    }
    let lsize = size_field(logical_size)?;
    let psize = size_field(physical_size)?;
    let object_type =
        u64::from(u8::try_from(object_type).context("block object type exceeds 255")?);
    Ok(lsize
        | (psize << 16)
        | (u64::from(compression) << 32)
        | (object_type << 48)
        | (u64::from(level) << 56)
        | (1_u64 << 61)
        | (byteorder << 63))
}

fn size_field(size: u64) -> Result<u64> {
    if size < 512 || !size.is_multiple_of(512) || size / 512 > 65_536 {
        bail!("raw block has invalid ZFS block size {size}");
    }
    Ok(size / 512 - 1)
}

pub fn decompress_block(compression: u8, payload: &[u8], logical_size: u64) -> Result<Vec<u8>> {
    // Some raw-send producers use zero in DRR_WRITE to mean an uncompressed
    // payload even though an on-disk blkptr stores ZIO_COMPRESS_OFF as two.
    let compression = if compression == 0 { 2 } else { compression };
    crate::compression::decompress_block(compression, payload, logical_size)
}

fn decrypt_aead(
    suite: u64,
    key: &[u8],
    iv: &[u8; 12],
    aad: &[u8],
    buffer: &mut [u8],
    mac: &[u8; 16],
) -> std::result::Result<(), ()> {
    let nonce = GenericArray::from_slice(iv);
    let tag = GenericArray::from_slice(mac);
    let result = match suite {
        3 => Ccm::<Aes128, U16, U12>::new_from_slice(key)
            .map_err(|_| ())?
            .decrypt_in_place_detached(nonce, aad, buffer, tag),
        4 => Ccm::<Aes192, U16, U12>::new_from_slice(key)
            .map_err(|_| ())?
            .decrypt_in_place_detached(nonce, aad, buffer, tag),
        5 => Ccm::<Aes256, U16, U12>::new_from_slice(key)
            .map_err(|_| ())?
            .decrypt_in_place_detached(nonce, aad, buffer, tag),
        6 => Aes128Gcm::new_from_slice(key)
            .map_err(|_| ())?
            .decrypt_in_place_detached(nonce, aad, buffer, tag),
        7 => AesGcm::<Aes192, U12>::new_from_slice(key)
            .map_err(|_| ())?
            .decrypt_in_place_detached(nonce, aad, buffer, tag),
        8 => Aes256Gcm::new_from_slice(key)
            .map_err(|_| ())?
            .decrypt_in_place_detached(nonce, aad, buffer, tag),
        _ => return Err(()),
    };
    result.map_err(|_| ())
}

fn suite_key_len(suite: u64) -> Result<usize> {
    match suite {
        3 | 6 => Ok(16),
        4 | 7 => Ok(24),
        5 | 8 => Ok(32),
        value => bail!("unsupported ZFS encryption suite value {value}"),
    }
}

fn decode_hex_key(material: &[u8], output: &mut [u8; 32]) -> Result<()> {
    if material.len() != 64 {
        bail!("hex ZFS key must contain exactly 64 hexadecimal characters");
    }
    for (index, pair) in material.chunks_exact(2).enumerate() {
        let text = std::str::from_utf8(pair).context("hex ZFS key is not UTF-8")?;
        output[index] = u8::from_str_radix(text, 16)
            .with_context(|| format!("invalid hexadecimal byte at key position {}", index * 2))?;
    }
    Ok(())
}

fn uint64(values: &BTreeMap<String, NvValue>, name: &str) -> Result<u64> {
    match values.get(name) {
        Some(NvValue::Uint64(value)) => Ok(*value),
        _ => bail!("raw stream crypt_keydata has no uint64 {name:?}"),
    }
}

fn bytes<'a>(values: &'a BTreeMap<String, NvValue>, name: &str) -> Result<&'a [u8]> {
    match values.get(name) {
        Some(NvValue::Bytes(value)) => Ok(value),
        _ => bail!("raw stream crypt_keydata has no byte array {name:?}"),
    }
}

fn array<const N: usize>(bytes: &[u8], name: &str) -> Result<[u8; N]> {
    bytes
        .try_into()
        .with_context(|| format!("raw stream {name} has length {}, expected {N}", bytes.len()))
}

struct NvDecoder<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> NvDecoder<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    fn decode(mut self) -> Result<BTreeMap<String, NvValue>> {
        let header: [u8; 4] = self.take(4)?.try_into().expect("four bytes");
        match header {
            [0, 1, 0, 0] => self.native_list(true),
            [1, _, 0, 0] => self.xdr_list(),
            [0, 0, 0, 0] => {
                bail!("big-endian native nvlists in raw streams are unsupported")
            }
            _ => bail!("raw stream crypt_keydata has an unsupported nvlist encoding"),
        }
    }

    fn xdr_list(&mut self) -> Result<BTreeMap<String, NvValue>> {
        let version = self.u32()?;
        let _flags = self.u32()?;
        if version != 0 {
            bail!("unsupported nvlist version {version}");
        }
        let mut values = BTreeMap::new();
        loop {
            let pair_start = self.cursor;
            let encoded_size = self.u32()? as usize;
            let _decoded_size = self.u32()?;
            if encoded_size == 0 {
                break;
            }
            if encoded_size < 8 {
                bail!("nvpair has an invalid encoded size");
            }
            let pair_end = pair_start
                .checked_add(encoded_size)
                .ok_or_else(|| anyhow!("nvpair size overflow"))?;
            if pair_end > self.bytes.len() {
                bail!("nvpair extends past the BEGIN payload");
            }
            let name = self.string()?;
            let value_type = self.u32()?;
            let count = self.u32()?;
            let value = match value_type {
                8 if count == 1 => NvValue::Uint64(self.u64()?),
                9 if count == 1 => {
                    self.string()?;
                    NvValue::Ignored
                }
                19 if count == 1 => NvValue::List(self.xdr_list()?),
                26 => {
                    let wire_count = self.u32()?;
                    if wire_count != count {
                        bail!("nvpair byte-array count does not match its XDR count");
                    }
                    let mut bytes = Vec::with_capacity(count as usize);
                    for _ in 0..count {
                        bytes.push(self.u32()? as u8);
                    }
                    NvValue::Bytes(bytes)
                }
                _ => bail!("unsupported nvpair type {value_type} with {count} elements"),
            };
            if self.cursor > pair_end {
                bail!("nvpair payload exceeds its encoded size");
            }
            self.cursor = pair_end;
            values.insert(name, value);
        }
        Ok(values)
    }

    fn native_list(&mut self, root: bool) -> Result<BTreeMap<String, NvValue>> {
        if root {
            let version = self.le_u32()?;
            let _flags = self.le_u32()?;
            if version != 0 {
                bail!("unsupported native nvlist version {version}");
            }
        }

        let mut values = BTreeMap::new();
        loop {
            let pair_start = self.cursor;
            let pair_size = self.le_u32()? as usize;
            if pair_size == 0 {
                break;
            }
            if pair_size < 16 {
                bail!("native nvpair has an invalid encoded size");
            }
            let pair_end = pair_start
                .checked_add(pair_size)
                .ok_or_else(|| anyhow!("native nvpair size overflow"))?;
            if pair_end > self.bytes.len() {
                bail!("native nvpair extends past the BEGIN payload");
            }
            let name_size = self.le_u16()? as usize;
            self.take(2)?; // reserved
            let count = self.le_u32()?;
            let value_type = self.le_u32()?;
            if name_size == 0 {
                bail!("native nvpair has an empty name");
            }
            let name_bytes = self.take(name_size)?;
            if name_bytes.last() != Some(&0) {
                bail!("native nvpair name is not NUL terminated");
            }
            let name = std::str::from_utf8(&name_bytes[..name_size - 1])
                .context("native nvpair name is not UTF-8")?
                .to_owned();
            let relative = self.cursor - pair_start;
            self.cursor = pair_start
                .checked_add(align_8(relative).ok_or_else(|| anyhow!("nvpair alignment overflow"))?)
                .ok_or_else(|| anyhow!("nvpair alignment overflow"))?;
            if self.cursor > pair_end {
                bail!("native nvpair name exceeds the pair size");
            }

            let value = match value_type {
                8 if count == 1 => NvValue::Uint64(self.le_u64()?),
                9 if count == 1 => NvValue::Ignored,
                19 if count == 1 => {
                    // The pair contains a 24-byte nvlist_t placeholder. Its
                    // nested pairs follow the containing nvpair on the wire.
                    if pair_end.saturating_sub(self.cursor) < 24 {
                        bail!("embedded native nvlist header is truncated");
                    }
                    let version = self.le_u32()?;
                    if version != 0 {
                        bail!("unsupported embedded native nvlist version {version}");
                    }
                    self.cursor = pair_end;
                    NvValue::List(self.native_list(false)?)
                }
                26 => {
                    let count = usize::try_from(count).context("native byte array is too large")?;
                    NvValue::Bytes(self.take(count)?.to_vec())
                }
                _ => bail!("unsupported native nvpair type {value_type} with {count} elements"),
            };
            if self.cursor < pair_end {
                self.cursor = pair_end;
            } else if self.cursor > pair_end && !matches!(value_type, 19) {
                bail!("native nvpair payload exceeds its encoded size");
            }
            values.insert(name, value);
        }
        Ok(values)
    }

    fn string(&mut self) -> Result<String> {
        let len = self.u32()? as usize;
        let bytes = self.take(len)?;
        let value = std::str::from_utf8(bytes)
            .context("nvlist string is not UTF-8")?
            .to_owned();
        let padding = (4 - len % 4) % 4;
        self.take(padding)?;
        Ok(value)
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().expect("four bytes"),
        ))
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().expect("eight bytes"),
        ))
    }

    fn le_u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().expect("two bytes"),
        ))
    }

    fn le_u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("four bytes"),
        ))
    }

    fn le_u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("eight bytes"),
        ))
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .cursor
            .checked_add(len)
            .ok_or_else(|| anyhow!("nvlist offset overflow"))?;
        if end > self.bytes.len() {
            bail!("truncated nvlist in raw stream BEGIN payload");
        }
        let value = &self.bytes[self.cursor..end];
        self.cursor = end;
        Ok(value)
    }
}

fn align_8(value: usize) -> Option<usize> {
    value.checked_add(7).map(|value| value & !7)
}

#[cfg(test)]
mod tests {
    use super::decode_hex_key;

    #[test]
    fn hex_key_requires_exactly_32_bytes() {
        let mut output = [0_u8; 32];
        decode_hex_key(
            b"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            &mut output,
        )
        .unwrap();
        assert_eq!(output[0], 0);
        assert_eq!(output[31], 31);
    }
}
