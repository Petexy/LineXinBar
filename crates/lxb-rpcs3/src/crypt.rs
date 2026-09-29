//! The two ways the PlayStation 3 uses AES-128, and the keys everybody who
//! reads its formats uses with them.
//!
//! Neither mode is a crate here: both are a few lines over the block cipher,
//! and taking the block cipher alone keeps this helper's tree to one new
//! package. What they have in common is that the PS3 never authenticates what
//! it encrypts, so neither of these can say a key was wrong — the caller finds
//! that out by looking at what came back (a package's file table that makes no
//! sense, a disc sector without the magic its file starts with).

use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use aes::Aes128;

/// The key every retail PS3 package's contents are encrypted under.
///
/// Published in RPCS3's `Crypto/key_vault.h` as `PKG_AES_KEY`, and the same in
/// every package tool since 2010. Retail packages are encrypted with it in
/// counter mode, the counter starting at the header's own sixteen-byte nonce.
pub const PKG_KEY: [u8; 16] = [
    0x2e, 0x7b, 0x71, 0xd7, 0xc9, 0xc9, 0xa1, 0x4e, 0xa3, 0x22, 0x1f, 0x18, 0x88, 0x28, 0xb8, 0xf8,
];

/// The key an in-store demonstration unit's packages use instead — RPCS3's
/// `PKG_AES_KEY_IDU`. Tried second, exactly as RPCS3 tries it: a table that does
/// not decrypt under the retail key is asked again under this one.
pub const PKG_KEY_IDU: [u8; 16] = [
    0x5d, 0xb9, 0x11, 0xe6, 0xb7, 0xe5, 0x0a, 0x7d, 0x32, 0x15, 0x38, 0xfd, 0x7c, 0x66, 0xf1, 0x7b,
];

/// Counter-mode keystream XORed into `data`, which sits at `offset` bytes into
/// a stream whose counter starts at `nonce`.
///
/// `offset` need not be on a block boundary: a package's file names are
/// scattered through its data at arbitrary offsets, and a caller reading one of
/// them should not have to round down and slice. The counter is the nonce plus
/// the block number, as a 128-bit big-endian integer that wraps.
pub fn ctr(key: &[u8; 16], nonce: &[u8; 16], offset: u64, data: &mut [u8]) {
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let start = u128::from_be_bytes(*nonce);
    let mut block = offset / 16;
    let mut skip = (offset % 16) as usize;
    let mut at = 0;
    while at < data.len() {
        let counter = start.wrapping_add(u128::from(block)).to_be_bytes();
        let mut stream = GenericArray::clone_from_slice(&counter);
        cipher.encrypt_block(&mut stream);
        let take = (16 - skip).min(data.len() - at);
        for i in 0..take {
            data[at + i] ^= stream[skip + i];
        }
        at += take;
        skip = 0;
        block += 1;
    }
}

/// One Blu-ray sector decrypted in place: AES-128-CBC under the disc's key,
/// with the sector's own number as the initialisation vector.
///
/// The vector is sixteen bytes, zero but for the last four, which are the
/// sector number big-endian — RPCS3's `reset_iv` in `Loader/ISO.cpp`. `data`
/// is a whole number of blocks; anything past the last whole block is left
/// alone.
pub fn sector(key: &[u8; 16], lba: u32, data: &mut [u8]) {
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let mut chain = sector_iv(lba);
    for block in data.chunks_exact_mut(16) {
        // The next block is chained to this one's ciphertext, so it is kept
        // before the block is overwritten with what it decrypts to.
        let ciphertext: [u8; 16] = (&*block).try_into().expect("a whole block");
        let mut here = GenericArray::from(ciphertext);
        cipher.decrypt_block(&mut here);
        for (plain, (dec, prev)) in block.iter_mut().zip(here.iter().zip(chain.iter())) {
            *plain = dec ^ prev;
        }
        chain = ciphertext;
    }
}

/// The first sixteen bytes of a sector as they would read under `key`.
///
/// All that deciding whether a key is a disc's own ever needs: the first block
/// of a CBC stream depends only on the key and the vector, and the magic every
/// file this looks for starts with fits in it.
pub fn first_block(key: &[u8; 16], lba: u32, ciphertext: &[u8; 16]) -> [u8; 16] {
    let mut block = *ciphertext;
    sector(key, lba, &mut block);
    block
}

fn sector_iv(lba: u32) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[12..].copy_from_slice(&lba.to_be_bytes());
    iv
}

#[cfg(test)]
pub fn encrypt_sector(key: &[u8; 16], lba: u32, data: &mut [u8]) {
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let mut chain = sector_iv(lba);
    for block in data.chunks_exact_mut(16) {
        for (plain, prev) in block.iter_mut().zip(chain.iter()) {
            *plain ^= prev;
        }
        let mut here = GenericArray::clone_from_slice(block);
        cipher.encrypt_block(&mut here);
        block.copy_from_slice(&here);
        chain.copy_from_slice(block);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FIPS-197's own AES-128 example, through the counter mode with a zero
    /// counter: the first block of keystream is the block cipher applied to
    /// the nonce, which is the published ciphertext.
    #[test]
    fn counter_mode_starts_with_the_block_cipher_of_the_nonce() {
        let key: [u8; 16] = *b"\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0a\x0b\x0c\x0d\x0e\x0f";
        let nonce: [u8; 16] = *b"\x00\x11\x22\x33\x44\x55\x66\x77\x88\x99\xaa\xbb\xcc\xdd\xee\xff";
        let mut data = [0u8; 16];
        ctr(&key, &nonce, 0, &mut data);
        let expected = [
            0x69, 0xc4, 0xe0, 0xd8, 0x6a, 0x7b, 0x04, 0x30, 0xd8, 0xcd, 0xb7, 0x80, 0x70, 0xb4,
            0xc5, 0x5a,
        ];
        assert_eq!(data, expected);
    }

    /// Reading from the middle of a block gives the same bytes as reading the
    /// whole stream and slicing it — which is how a package's file names are
    /// read.
    #[test]
    fn counter_mode_can_start_anywhere() {
        let key = PKG_KEY;
        let nonce = [7u8; 16];
        let mut whole = vec![0u8; 100];
        ctr(&key, &nonce, 0, &mut whole);
        for start in [0usize, 1, 15, 16, 17, 33, 63] {
            let mut part = vec![0u8; 100 - start];
            ctr(&key, &nonce, start as u64, &mut part);
            assert_eq!(part, whole[start..], "from byte {start}");
        }
    }

    /// The counter is a 128-bit number and carries across all sixteen bytes.
    #[test]
    fn the_counter_wraps_as_one_number() {
        let key = PKG_KEY;
        let mut nonce = [0xffu8; 16];
        nonce[0] = 0x00;
        let mut across = [0u8; 32];
        ctr(&key, &nonce, 0, &mut across);
        let mut next = [0u8; 16];
        let mut carried = [0u8; 16];
        carried[0] = 0x01;
        ctr(&key, &carried, 0, &mut next);
        assert_eq!(across[16..], next);
    }

    #[test]
    fn a_sector_decrypts_what_was_encrypted_under_its_own_number() {
        let key = [0x42u8; 16];
        let original: Vec<u8> = (0..2048u32).map(|i| (i * 7 % 251) as u8).collect();
        let mut data = original.clone();
        encrypt_sector(&key, 0x940, &mut data);
        assert_ne!(data, original);

        let mut wrong_sector = data.clone();
        sector(&key, 0x941, &mut wrong_sector);
        assert_ne!(wrong_sector[..16], original[..16]);

        let head: [u8; 16] = data[..16].try_into().unwrap();
        assert_eq!(first_block(&key, 0x940, &head), original[..16]);
        sector(&key, 0x940, &mut data);
        assert_eq!(data, original);
    }
}
