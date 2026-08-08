//! Read-only adapters for Datto Reverse RoundTrip backup media.
//!
//! The outer LUKS container is handled by the pool reader. This module handles
//! the optional per-agent `.detto` layer: a 64-byte AES-XTS key authenticated
//! and recovered from the agent's compact JWE key stash.

use crate::inception::ImageRead;
use aes::Aes256;
use aes::cipher::{KeyInit, generic_array::GenericArray};
use aes_gcm::aead::AeadInPlace;
use aes_gcm::{Aes256Gcm, Nonce, Tag};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use pbkdf2::pbkdf2_hmac;
use serde::Deserialize;
use sha3::Sha3_256;
use std::sync::Arc;
use xts_mode::{Xts128, get_tweak_default};
use zeroize::{Zeroize, Zeroizing};

const DETTO_SECTOR_SIZE: u64 = 512;
const MAX_PBKDF2_ITERATIONS: u32 = 10_000_000;

#[derive(Deserialize)]
struct KeyStash {
    user_keys_jwe: Vec<String>,
}

#[derive(Deserialize)]
struct ProtectedHeader {
    p2s: String,
    p2c: u32,
}

#[derive(Deserialize)]
struct MasterKey {
    k: String,
}

fn decode_base64(value: &str, label: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(value)
        .with_context(|| format!("decoding {label} as base64url"))
}

/// Authenticate a Datto agent key stash and recover its 64-byte AES-XTS key.
pub fn derive_agent_key(stash: &[u8], password: &[u8]) -> Result<Zeroizing<[u8; 64]>> {
    if password.is_empty() {
        bail!("the Datto agent password is empty");
    }
    let stash: KeyStash = serde_json::from_slice(stash).context("parsing Datto key stash JSON")?;
    let token = stash
        .user_keys_jwe
        .first()
        .ok_or_else(|| anyhow!("Datto key stash has no user_keys_jwe entry"))?;
    let parts = token.split('.').collect::<Vec<_>>();
    let [protected, encrypted_key, iv, ciphertext, tag] = parts.as_slice() else {
        bail!("Datto key stash JWE must contain five compact fields");
    };
    // Datto's current direct-key form leaves this field empty. The published
    // recovery procedure ignores it, so compatibility follows the authenticated
    // ciphertext rather than rejecting a future harmless serialization value.
    let _ = encrypted_key;

    let protected_bytes = decode_base64(protected, "Datto protected header")?;
    let header: ProtectedHeader =
        serde_json::from_slice(&protected_bytes).context("parsing Datto protected header")?;
    if header.p2c == 0 || header.p2c > MAX_PBKDF2_ITERATIONS {
        bail!(
            "Datto PBKDF2 iteration count {} is outside 1..={MAX_PBKDF2_ITERATIONS}",
            header.p2c
        );
    }
    let salt = decode_base64(&header.p2s, "Datto PBKDF2 salt")?;
    let iv = decode_base64(iv, "Datto JWE IV")?;
    let tag = decode_base64(tag, "Datto JWE authentication tag")?;
    if iv.len() != 12 || tag.len() != 16 {
        bail!("Datto JWE requires a 12-byte IV and 16-byte authentication tag");
    }

    let mut wrapping_key = Zeroizing::new([0_u8; 32]);
    pbkdf2_hmac::<Sha3_256>(password, &salt, header.p2c, &mut wrapping_key[..]);
    let cipher = Aes256Gcm::new_from_slice(&wrapping_key[..])
        .map_err(|_| anyhow!("constructing Datto AES-256-GCM key"))?;
    let mut plaintext = decode_base64(ciphertext, "Datto JWE ciphertext")?;
    cipher
        .decrypt_in_place_detached(
            Nonce::from_slice(&iv),
            protected.as_bytes(),
            &mut plaintext,
            Tag::from_slice(&tag),
        )
        .map_err(|_| anyhow!("the Datto agent password did not authenticate the key stash"))?;

    let mut master: MasterKey =
        serde_json::from_slice(&plaintext).context("parsing decrypted Datto master key")?;
    plaintext.zeroize();
    let decoded = Zeroizing::new(decode_base64(&master.k, "Datto AES-XTS master key")?);
    master.k.zeroize();
    if decoded.len() != 64 {
        bail!("Datto AES-XTS key is {} bytes, not 64", decoded.len());
    }
    let mut key = [0_u8; 64];
    key.copy_from_slice(&decoded);
    Ok(Zeroizing::new(key))
}

/// Positioned plaintext view of an AES-256-XTS `.detto` image.
pub(crate) struct DettoImage {
    source: Arc<dyn ImageRead>,
    key: Zeroizing<[u8; 64]>,
}

impl DettoImage {
    pub(crate) fn new(source: Arc<dyn ImageRead>, key: Zeroizing<[u8; 64]>) -> Result<Self> {
        if source.len() == 0 || !source.len().is_multiple_of(DETTO_SECTOR_SIZE) {
            bail!(".detto image size must be a non-zero multiple of 512 bytes");
        }
        Ok(Self { source, key })
    }
}

impl ImageRead for DettoImage {
    fn len(&self) -> u64 {
        self.source.len()
    }

    fn read_exact_at(&self, offset: u64, buffer: &mut [u8]) -> Result<()> {
        let end = offset
            .checked_add(buffer.len() as u64)
            .ok_or_else(|| anyhow!(".detto read offset overflows"))?;
        if end > self.source.len() {
            bail!(
                ".detto read [{offset}, {end}) exceeds {} bytes",
                self.source.len()
            );
        }
        if buffer.is_empty() {
            return Ok(());
        }
        let aligned_start = offset / DETTO_SECTOR_SIZE * DETTO_SECTOR_SIZE;
        let aligned_end = end.div_ceil(DETTO_SECTOR_SIZE) * DETTO_SECTOR_SIZE;
        let mut ciphertext = vec![0_u8; usize::try_from(aligned_end - aligned_start)?];
        self.source.read_exact_at(aligned_start, &mut ciphertext)?;

        let first = GenericArray::from_slice(&self.key[..32]);
        let second = GenericArray::from_slice(&self.key[32..]);
        let xts = Xts128::<Aes256>::new(Aes256::new(first), Aes256::new(second));
        xts.decrypt_area(
            &mut ciphertext,
            DETTO_SECTOR_SIZE as usize,
            u128::from(aligned_start / DETTO_SECTOR_SIZE),
            get_tweak_default,
        );
        let within = usize::try_from(offset - aligned_start)?;
        buffer.copy_from_slice(&ciphertext[within..within + buffer.len()]);
        ciphertext.zeroize();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{DettoImage, derive_agent_key};
    use crate::inception::ImageRead;
    use aes::Aes256;
    use aes::cipher::generic_array::GenericArray;
    use aes_gcm::aead::AeadInPlace;
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use pbkdf2::pbkdf2_hmac;
    use sha3::Sha3_256;
    use std::sync::Arc;
    use xts_mode::{Xts128, get_tweak_default};
    use zeroize::Zeroizing;

    struct Bytes(Vec<u8>);

    impl ImageRead for Bytes {
        fn len(&self) -> u64 {
            self.0.len() as u64
        }

        fn read_exact_at(&self, offset: u64, buffer: &mut [u8]) -> anyhow::Result<()> {
            let start = offset as usize;
            buffer.copy_from_slice(&self.0[start..start + buffer.len()]);
            Ok(())
        }
    }

    #[test]
    fn key_stash_round_trip_authenticates_and_recovers_xts_key() {
        let password = b"agent-passphrase";
        let salt = b"datto-test-salt";
        let header = format!(r#"{{"p2s":"{}","p2c":4096}}"#, URL_SAFE_NO_PAD.encode(salt));
        let protected = URL_SAFE_NO_PAD.encode(header);
        let mut wrapping = [0_u8; 32];
        pbkdf2_hmac::<Sha3_256>(password, salt, 4096, &mut wrapping);
        let cipher = Aes256Gcm::new_from_slice(&wrapping).unwrap();
        let nonce = [7_u8; 12];
        let expected = [0x5a_u8; 64];
        let mut plaintext =
            format!(r#"{{"k":"{}"}}"#, URL_SAFE_NO_PAD.encode(expected)).into_bytes();
        let tag = cipher
            .encrypt_in_place_detached(
                Nonce::from_slice(&nonce),
                protected.as_bytes(),
                &mut plaintext,
            )
            .unwrap();
        let token = format!(
            "{}..{}.{}.{}",
            protected,
            URL_SAFE_NO_PAD.encode(nonce),
            URL_SAFE_NO_PAD.encode(plaintext),
            URL_SAFE_NO_PAD.encode(tag)
        );
        let stash = format!(r#"{{"user_keys_jwe":["{token}"]}}"#);
        assert_eq!(
            &derive_agent_key(stash.as_bytes(), password).unwrap()[..],
            &expected
        );
        assert!(derive_agent_key(stash.as_bytes(), b"wrong").is_err());
    }

    #[test]
    fn detto_reader_decrypts_unaligned_positioned_ranges() {
        let key = [0x3c_u8; 64];
        let plaintext = (0..1536).map(|value| value as u8).collect::<Vec<_>>();
        let mut ciphertext = plaintext.clone();
        let first = GenericArray::from_slice(&key[..32]);
        let second = GenericArray::from_slice(&key[32..]);
        let xts = Xts128::<Aes256>::new(Aes256::new(first), Aes256::new(second));
        xts.encrypt_area(&mut ciphertext, 512, 0, get_tweak_default);
        let image = DettoImage::new(Arc::new(Bytes(ciphertext)), Zeroizing::new(key)).unwrap();
        let mut range = vec![0_u8; 777];
        image.read_exact_at(333, &mut range).unwrap();
        assert_eq!(range, plaintext[333..1110]);
    }
}
