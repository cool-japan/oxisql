//! RFC 9001 §5.4 QUIC header protection implementation.
//!
//! Header protection derives a 5-byte mask from a 16-byte sample of packet
//! ciphertext, then XORs specific bits of the first byte and the packet-number
//! bytes.  The operation is identical for encryption and decryption (XOR is
//! self-inverse); only the order of reading the packet-number length differs.
#![allow(clippy::duplicate_mod)]

#[cfg(feature = "alloc")]
use alloc::boxed::Box;

use aead::AeadCore;
use chacha20poly1305::{AeadInPlace, KeyInit, KeySizeUser};
use crypto_common::typenum::Unsigned;
use rustls::crypto::cipher::{self, AeadKey, Iv};
use rustls::{quic, Error, Tls13CipherSuite};

// ── AES re-exports via aes-gcm ────────────────────────────────────────────────
use aes_gcm::aes::{
    Aes128, Aes256,
    cipher::{BlockEncrypt, generic_array::GenericArray},
};

// ── ChaCha20 stream cipher ────────────────────────────────────────────────────
use chacha20::{
    ChaCha20 as ChaCha20Core,
    cipher::{KeyIvInit, StreamCipher, StreamCipherSeek},
};

/// Size of the header-protection sample in bytes (RFC 9001 §5.4.2).
const SAMPLE_LEN: usize = 16;

/// Maximum packet-number length in bytes (RFC 9000 §17.1).
const MAX_PN_LEN: usize = 4;

/// Number of mask bytes produced (RFC 9001 §5.4.1: 1 first-byte + 4 PN bytes).
const MASK_LEN: usize = 5;

// ── Cipher-suite discriminant ─────────────────────────────────────────────────

/// Which cipher to use for header protection.
///
/// RFC 9001 §5.4.3: AES-based header protection uses AES-ECB (one block).
/// RFC 9001 §5.4.4: ChaCha20-based header protection uses ChaCha20 keystream.
#[derive(Clone, Copy, Debug)]
pub enum HeaderCipher {
    /// AES-128-ECB; key is 16 bytes.
    Aes128,
    /// AES-256-ECB; key is 32 bytes.
    Aes256,
    /// ChaCha20; key is 32 bytes.
    ChaCha20,
}

// ── HeaderProtectionKey ───────────────────────────────────────────────────────

/// QUIC header-protection key (RFC 9001 §5.4).
pub struct HeaderProtectionKey {
    key: AeadKey,
    cipher: HeaderCipher,
}

impl HeaderProtectionKey {
    /// Construct a new header-protection key with the specified cipher variant.
    pub fn new(key: AeadKey, cipher: HeaderCipher) -> Self {
        Self { key, cipher }
    }

    /// Derive the 5-byte mask from `sample` using the configured cipher.
    ///
    /// Returns `Err` if `sample` is not exactly `SAMPLE_LEN` bytes long.
    fn derive_mask(&self, sample: &[u8]) -> Result<[u8; MASK_LEN], Error> {
        if sample.len() != SAMPLE_LEN {
            return Err(Error::General(
                "QUIC header protection: sample must be 16 bytes".into(),
            ));
        }

        match self.cipher {
            HeaderCipher::Aes128 => {
                // RFC 9001 §5.4.3 — AES-128-ECB
                let key_bytes = self.key.as_ref();
                if key_bytes.len() < 16 {
                    return Err(Error::General(
                        "QUIC header protection: AES-128 key must be 16 bytes".into(),
                    ));
                }
                let cipher = Aes128::new(GenericArray::from_slice(&key_bytes[..16]));
                let mut block: GenericArray<u8, _> = GenericArray::clone_from_slice(sample);
                cipher.encrypt_block(&mut block);
                let mask: [u8; MASK_LEN] = block[..MASK_LEN]
                    .try_into()
                    .map_err(|_| Error::General("invariant: AES block >= 5 bytes".into()))?;
                Ok(mask)
            }

            HeaderCipher::Aes256 => {
                // RFC 9001 §5.4.3 — AES-256-ECB
                let key_bytes = self.key.as_ref();
                if key_bytes.len() < 32 {
                    return Err(Error::General(
                        "QUIC header protection: AES-256 key must be 32 bytes".into(),
                    ));
                }
                let cipher = Aes256::new(GenericArray::from_slice(&key_bytes[..32]));
                let mut block: GenericArray<u8, _> = GenericArray::clone_from_slice(sample);
                cipher.encrypt_block(&mut block);
                let mask: [u8; MASK_LEN] = block[..MASK_LEN]
                    .try_into()
                    .map_err(|_| Error::General("invariant: AES block >= 5 bytes".into()))?;
                Ok(mask)
            }

            HeaderCipher::ChaCha20 => {
                // RFC 9001 §5.4.4 — ChaCha20 header protection
                // counter = sample[0..4] as little-endian u32
                // nonce   = sample[4..16]
                let counter_bytes: [u8; 4] = sample[0..4]
                    .try_into()
                    .map_err(|_| Error::General("invariant: sample >= 4 bytes".into()))?;
                let counter = u32::from_le_bytes(counter_bytes);

                let nonce_bytes: &[u8; 12] = sample[4..16]
                    .try_into()
                    .map_err(|_| Error::General("invariant: sample[4..16] is 12 bytes".into()))?;

                let key_bytes = self.key.as_ref();
                if key_bytes.len() < 32 {
                    return Err(Error::General(
                        "QUIC header protection: ChaCha20 key must be 32 bytes".into(),
                    ));
                }

                let key_array: &[u8; 32] = key_bytes[..32]
                    .try_into()
                    .map_err(|_| Error::General("invariant: key >= 32 bytes".into()))?;

                let mut stream =
                    ChaCha20Core::new(key_array.into(), nonce_bytes.into());

                // `seek` takes a byte offset; each block is 64 bytes.
                // The counter in RFC 9001 §5.4.4 is a block counter.
                let byte_offset: u64 = u64::from(counter)
                    .checked_mul(64)
                    .ok_or_else(|| Error::General("QUIC header protection: counter overflow".into()))?;
                stream
                    .try_seek(byte_offset)
                    .map_err(|_| Error::General("QUIC header protection: seek failed".into()))?;

                let mut mask = [0u8; MASK_LEN];
                stream.apply_keystream(&mut mask);
                Ok(mask)
            }
        }
    }

    /// Apply (or remove) header protection in-place using the given mask.
    ///
    /// `masked` controls whether we read the packet-number length *before* or
    /// *after* unmasking the first byte:
    ///   - `false` (encrypt): read from original `first` (before modification).
    ///   - `true`  (decrypt): read from `first ^ mask_byte` (after unmasking).
    ///
    /// Both operations are otherwise identical XOR applications.
    fn xor_in_place(
        mask: &[u8; MASK_LEN],
        first: &mut u8,
        packet_number: &mut [u8],
        masked: bool,
    ) -> Result<(), Error> {
        if packet_number.len() > MAX_PN_LEN {
            return Err(Error::General(
                "QUIC header protection: packet number too long".into(),
            ));
        }

        // RFC 9001 §5.4.1 — Header Protection Application
        const LONG_HEADER_FORM: u8 = 0x80;
        let bits = if *first & LONG_HEADER_FORM == LONG_HEADER_FORM {
            0x0f_u8 // long header: 4 bits masked
        } else {
            0x1f_u8 // short header: 5 bits masked
        };

        let (first_mask, pn_mask) = mask.split_first().expect("invariant: MASK_LEN >= 1");

        // When decrypting (unmasking), read PN length from the already-unmasked byte.
        let first_plain = if masked {
            *first ^ (first_mask & bits)
        } else {
            *first
        };
        let pn_len = (first_plain & 0x03) as usize + 1;

        // Infallible from this point — first and packet_number are now mutated.
        *first ^= first_mask & bits;
        for (dst, m) in packet_number.iter_mut().zip(pn_mask.iter()).take(pn_len) {
            *dst ^= m;
        }

        Ok(())
    }
}

impl quic::HeaderProtectionKey for HeaderProtectionKey {
    /// Encrypt (protect) the QUIC packet header in-place.
    ///
    /// Reads the packet-number length from `first` *before* masking,
    /// then XORs `first` and `packet_number` with the derived mask.
    fn encrypt_in_place(
        &self,
        sample: &[u8],
        first: &mut u8,
        packet_number: &mut [u8],
    ) -> Result<(), Error> {
        let mask = self.derive_mask(sample)?;
        Self::xor_in_place(&mask, first, packet_number, false)
    }

    /// Decrypt (remove protection from) the QUIC packet header in-place.
    ///
    /// Reads the packet-number length from `first` *after* unmasking,
    /// then XORs `first` and `packet_number` with the derived mask.
    fn decrypt_in_place(
        &self,
        sample: &[u8],
        first: &mut u8,
        packet_number: &mut [u8],
    ) -> Result<(), Error> {
        let mask = self.derive_mask(sample)?;
        Self::xor_in_place(&mask, first, packet_number, true)
    }

    /// Expected sample length for the key's algorithm (always 16 bytes).
    #[inline]
    fn sample_len(&self) -> usize {
        SAMPLE_LEN
    }
}

// ── PacketKey ─────────────────────────────────────────────────────────────────

/// QUIC packet key using ChaCha20-Poly1305.
pub struct PacketKey {
    /// Computes unique nonces for each packet
    iv: Iv,

    /// The cipher suite used for this packet key
    #[allow(dead_code)]
    suite: &'static Tls13CipherSuite,

    crypto: chacha20poly1305::ChaCha20Poly1305,
}

impl PacketKey {
    /// Construct a new packet key from the given AEAD key and IV.
    pub fn new(suite: &'static Tls13CipherSuite, key: AeadKey, iv: Iv) -> Self {
        Self {
            iv,
            suite,
            crypto: chacha20poly1305::ChaCha20Poly1305::new_from_slice(key.as_ref())
                .expect("key should be valid"),
        }
    }
}

impl quic::PacketKey for PacketKey {
    fn encrypt_in_place(
        &self,
        packet_number: u64,
        aad: &[u8],
        payload: &mut [u8],
    ) -> Result<quic::Tag, Error> {
        let nonce = cipher::Nonce::new(&self.iv, packet_number).0;

        let tag = self
            .crypto
            .encrypt_in_place_detached(&nonce.into(), aad, payload)
            .map_err(|_| rustls::Error::EncryptError)?;
        Ok(quic::Tag::from(tag.as_ref()))
    }

    /// Decrypt a QUIC packet
    ///
    /// Takes the packet `header`, which is used as the additional authenticated
    /// data, and the `payload`, which includes the authentication tag.
    ///
    /// If the return value is `Ok`, the decrypted payload can be found in
    /// `payload`, up to the length found in the return value.
    fn decrypt_in_place<'a>(
        &self,
        packet_number: u64,
        aad: &[u8],
        payload: &'a mut [u8],
    ) -> Result<&'a [u8], Error> {
        let mut payload_ = payload.to_vec();
        let payload_len = payload_.len();
        let nonce = chacha20poly1305::Nonce::from(cipher::Nonce::new(&self.iv, packet_number).0);

        self.crypto
            .decrypt_in_place(&nonce, aad, &mut payload_)
            .map_err(|_| rustls::Error::DecryptError)?;

        // Unfortunately the lifetime bound on decrypt_in_place sucks
        payload.copy_from_slice(&payload_);

        let plain_len = payload_len - self.tag_len();
        Ok(&payload[..plain_len])
    }

    /// Tag length for the underlying AEAD algorithm
    #[inline]
    fn tag_len(&self) -> usize {
        <chacha20poly1305::ChaCha20Poly1305 as AeadCore>::TagSize::to_usize()
    }

    fn integrity_limit(&self) -> u64 {
        1 << 36
    }

    fn confidentiality_limit(&self) -> u64 {
        u64::MAX
    }
}

// ── KeyBuilder ────────────────────────────────────────────────────────────────

/// QUIC algorithm builder that produces [`PacketKey`] and [`HeaderProtectionKey`].
pub struct KeyBuilder {
    /// Which header-protection cipher to instantiate.
    cipher: HeaderCipher,
    /// The cipher suite, carried through to [`PacketKey`].
    suite: &'static Tls13CipherSuite,
}

impl KeyBuilder {
    /// Construct a new builder for ChaCha20-Poly1305 QUIC keys.
    pub fn new_chacha20(suite: &'static Tls13CipherSuite) -> Self {
        Self {
            cipher: HeaderCipher::ChaCha20,
            suite,
        }
    }

    /// Construct a new builder for AES-128-GCM QUIC keys.
    pub fn new_aes128(suite: &'static Tls13CipherSuite) -> Self {
        Self {
            cipher: HeaderCipher::Aes128,
            suite,
        }
    }

    /// Construct a new builder for AES-256-GCM QUIC keys.
    pub fn new_aes256(suite: &'static Tls13CipherSuite) -> Self {
        Self {
            cipher: HeaderCipher::Aes256,
            suite,
        }
    }
}

impl rustls::quic::Algorithm for KeyBuilder {
    fn packet_key(&self, key: AeadKey, iv: Iv) -> Box<dyn quic::PacketKey> {
        Box::new(PacketKey::new(self.suite, key, iv))
    }

    fn header_protection_key(&self, key: AeadKey) -> Box<dyn quic::HeaderProtectionKey> {
        Box::new(HeaderProtectionKey::new(key, self.cipher))
    }

    fn aead_key_len(&self) -> usize {
        chacha20poly1305::ChaCha20Poly1305::key_size()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::crypto::cipher::AeadKey;
    // Bring trait methods (encrypt_in_place, decrypt_in_place, sample_len) into scope.
    use rustls::quic::HeaderProtectionKey as QuicHpk;

    // ── helpers ───────────────────────────────────────────────────────────────

    /// Build an `AeadKey` from a hex string (pads to MAX_LEN = 32 bytes).
    fn key_from_hex(hex: &str) -> AeadKey {
        let bytes = decode_hex(hex);
        let mut buf = [0u8; 32]; // AeadKey::MAX_LEN
        let copy_len = bytes.len().min(buf.len());
        buf[..copy_len].copy_from_slice(&bytes[..copy_len]);
        AeadKey::from(buf)
    }

    fn decode_hex(hex: &str) -> alloc::vec::Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| {
                u8::from_str_radix(&hex[i..i + 2], 16)
                    .expect("valid hex string")
            })
            .collect()
    }

    fn mask_from_hex(hex: &str) -> [u8; MASK_LEN] {
        let v = decode_hex(hex);
        v.try_into().expect("hex must be exactly 5 bytes (10 hex chars)")
    }

    // ── RFC 9001 Appendix A.2 — AES-128 test vector ───────────────────────────
    //
    // Header protection key : 9f50449e04a0e810283a1e9933adedd2
    // Sample                 : d1b1c98dd7689fb8ec11d242b123dc9b
    // Expected mask          : 437b9aec36

    #[test]
    fn test_aes128_mask_rfc9001_appendix_a2() {
        let hpk = HeaderProtectionKey::new(
            key_from_hex("9f50449e04a0e810283a1e9933adedd2"),
            HeaderCipher::Aes128,
        );
        let sample = decode_hex("d1b1c98dd7689fb8ec11d242b123dc9b");
        let mask = hpk.derive_mask(&sample).expect("derive_mask should succeed");
        assert_eq!(mask, mask_from_hex("437b9aec36"), "AES-128 mask mismatch");
    }

    // ── RFC 9001 Appendix A.5 — ChaCha20 test vector ─────────────────────────
    //
    // Header protection key : 25a282b9e82f06f21f488917a4fc8f1b73573685608597d0efcb076b0ab7a7a4
    // Sample                 : 5e5cd55c41f69080575d7999c25a5bfb
    // Expected mask          : aefefe7d03

    #[test]
    fn test_chacha20_mask_rfc9001_appendix_a5() {
        let hpk = HeaderProtectionKey::new(
            key_from_hex("25a282b9e82f06f21f488917a4fc8f1b73573685608597d0efcb076b0ab7a7a4"),
            HeaderCipher::ChaCha20,
        );
        let sample = decode_hex("5e5cd55c41f69080575d7999c25a5bfb");
        let mask = hpk.derive_mask(&sample).expect("derive_mask should succeed");
        assert_eq!(mask, mask_from_hex("aefefe7d03"), "ChaCha20 mask mismatch");
    }

    // ── Round-trip: encrypt then decrypt restores original header ─────────────

    fn roundtrip(cipher: HeaderCipher, key_hex: &str) {
        let hpk = HeaderProtectionKey::new(key_from_hex(key_hex), cipher);

        // Craft a synthetic short-header first byte and 4-byte packet number.
        // Short header: bit 7 == 0; bits 0-1 = 0b11 => pn_length = 4.
        let original_first: u8 = 0b0100_0011;
        let original_pn: [u8; 4] = [0xde, 0xad, 0xbe, 0xef];

        // Use the RFC 9001 AES-128 sample (always 16 bytes).
        let sample = decode_hex("d1b1c98dd7689fb8ec11d242b123dc9b");

        let mut first = original_first;
        let mut pn = original_pn;

        // Encrypt (protect).
        hpk.encrypt_in_place(&sample, &mut first, &mut pn)
            .expect("encrypt should succeed");

        // After encryption the bytes must differ.
        assert!(
            first != original_first || pn != original_pn,
            "encrypt must change something"
        );

        // Decrypt (remove protection).
        hpk.decrypt_in_place(&sample, &mut first, &mut pn)
            .expect("decrypt should succeed");

        assert_eq!(first, original_first, "first byte must be restored");
        assert_eq!(pn, original_pn, "packet number must be restored");
    }

    #[test]
    fn roundtrip_aes128() {
        roundtrip(
            HeaderCipher::Aes128,
            "9f50449e04a0e810283a1e9933adedd2",
        );
    }

    #[test]
    fn roundtrip_aes256() {
        roundtrip(
            HeaderCipher::Aes256,
            "9f50449e04a0e810283a1e9933adedd29f50449e04a0e810283a1e9933adedd2",
        );
    }

    #[test]
    fn roundtrip_chacha20() {
        roundtrip(
            HeaderCipher::ChaCha20,
            "25a282b9e82f06f21f488917a4fc8f1b73573685608597d0efcb076b0ab7a7a4",
        );
    }

    // ── sample_len ────────────────────────────────────────────────────────────

    #[test]
    fn sample_len_is_16_for_all_ciphers() {
        for cipher in [HeaderCipher::Aes128, HeaderCipher::Aes256, HeaderCipher::ChaCha20] {
            let hpk = HeaderProtectionKey::new(
                key_from_hex("9f50449e04a0e810283a1e9933adedd29f50449e04a0e810283a1e9933adedd2"),
                cipher,
            );
            assert_eq!(hpk.sample_len(), 16, "sample_len must be 16 for {:?}", cipher);
        }
    }

    // ── error cases ───────────────────────────────────────────────────────────

    #[test]
    fn derive_mask_rejects_short_sample() {
        let hpk = HeaderProtectionKey::new(
            key_from_hex("9f50449e04a0e810283a1e9933adedd2"),
            HeaderCipher::Aes128,
        );
        assert!(
            hpk.derive_mask(&[0u8; 15]).is_err(),
            "sample shorter than 16 bytes must fail"
        );
        assert!(
            hpk.derive_mask(&[0u8; 17]).is_err(),
            "sample longer than 16 bytes must fail"
        );
    }

    #[test]
    fn encrypt_rejects_long_packet_number() {
        let hpk = HeaderProtectionKey::new(
            key_from_hex("9f50449e04a0e810283a1e9933adedd2"),
            HeaderCipher::Aes128,
        );
        let sample = decode_hex("d1b1c98dd7689fb8ec11d242b123dc9b");
        let mut first: u8 = 0x43;
        let mut pn = [0u8; 5]; // 5 bytes — exceeds MAX_PN_LEN
        assert!(
            hpk.encrypt_in_place(&sample, &mut first, &mut pn).is_err(),
            "packet number > 4 bytes must fail"
        );
    }

    // ── long-header masking (bits 0x0f) ───────────────────────────────────────

    #[test]
    fn long_header_uses_0x0f_mask_bits() {
        let hpk = HeaderProtectionKey::new(
            key_from_hex("9f50449e04a0e810283a1e9933adedd2"),
            HeaderCipher::Aes128,
        );
        let sample = decode_hex("d1b1c98dd7689fb8ec11d242b123dc9b");

        // Long header: bit 7 set; pn_length = 1 (bits 0-1 = 0b00).
        let original_first: u8 = 0b1100_0000;
        let mut first = original_first;
        let mut pn = [0xAB_u8; 4];
        let original_pn = pn;

        hpk.encrypt_in_place(&sample, &mut first, &mut pn)
            .expect("encrypt should succeed for long-header test");
        hpk.decrypt_in_place(&sample, &mut first, &mut pn)
            .expect("decrypt should succeed for long-header test");

        assert_eq!(first, original_first);
        assert_eq!(pn, original_pn);
    }
}
