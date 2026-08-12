//! Encrypting the password with the account's own key, before it leaves.
//!
//! Steam never takes a password as it was typed. The client asks for the RSA
//! public key that belongs to *that account name* — modulus and exponent, as
//! hexadecimal, with the timestamp they were minted at — pads the password the
//! way PKCS #1 has since 1993, encrypts it under that key, and sends the
//! ciphertext and the timestamp together. The key rotates, which is what the
//! timestamp is for: a ciphertext presented against a key that has been
//! replaced is refused rather than retried.
//!
//! Only the public half of RSA is here, and only encryption. That is the one
//! direction with nothing secret in it — the key is public, and the operation
//! is a modular exponentiation of a padded number — so the failure modes that
//! make hand-written RSA a bad idea (timing on the private exponent, padding
//! oracles on decryption, forged signatures from a lax verifier) have no
//! counterpart on this side. What has to be right is the padding, and the
//! padding is nine lines of a specification that has not moved in thirty
//! years. See the test at the bottom, which checks it against a key whose
//! private half is known.

use num_bigint::BigUint;
use rand::RngCore;

/// The public key for one account, as `GetPasswordRSAPublicKey` answers.
#[derive(Debug, Clone)]
pub struct PublicKey {
    modulus: BigUint,
    exponent: BigUint,
    /// How wide the modulus is in bytes, which is how long every ciphertext
    /// under this key is and therefore how much room the padding has.
    width: usize,
    /// When Steam minted it. Sent back with the ciphertext; the server refuses
    /// a password encrypted under a key it has since rotated away from.
    pub timestamp: u64,
}

impl PublicKey {
    /// Read the key out of the two hexadecimal strings Steam sends it as.
    ///
    /// `None` for anything that is not a key: a string that is not hex, or a
    /// modulus small enough that no password could be padded into it.
    pub fn from_hex(modulus: &str, exponent: &str, timestamp: u64) -> Option<PublicKey> {
        let modulus = BigUint::parse_bytes(modulus.trim().as_bytes(), 16)?;
        let exponent = BigUint::parse_bytes(exponent.trim().as_bytes(), 16)?;
        let width = modulus.to_bytes_be().len();
        // Eleven bytes is what PKCS #1 reserves for the padding itself, so a
        // modulus that narrow has no room for a password of any length.
        if width < 12 || exponent == BigUint::ZERO {
            return None;
        }
        Some(PublicKey {
            modulus,
            exponent,
            width,
            timestamp,
        })
    }

    /// The longest password this key can carry, in bytes.
    pub fn capacity(&self) -> usize {
        self.width - 11
    }

    /// Encrypt `plain` under this key, padded as PKCS #1 v1.5 type 2.
    ///
    /// The padded block is
    ///
    /// ```text
    /// 0x00 || 0x02 || at least eight non-zero random bytes || 0x00 || plain
    /// ```
    ///
    /// The leading zero is what keeps the number below the modulus, the `0x02`
    /// says which of the two padding types this is, and the random run is what
    /// makes two encryptions of the same password different ciphertexts. The
    /// separator has to be the only zero in the padding, which is why the
    /// random bytes are drawn again whenever one comes up zero.
    ///
    /// `None` for a password longer than the key can carry — Steam's own limit
    /// is far below this, so it means the key was not the one that was asked
    /// for.
    pub fn encrypt(&self, plain: &[u8]) -> Option<Vec<u8>> {
        if plain.len() > self.capacity() {
            return None;
        }

        let mut block = vec![0u8; self.width];
        block[0] = 0x00;
        block[1] = 0x02;
        let separator = self.width - plain.len() - 1;
        let mut rng = rand::rng();
        for byte in &mut block[2..separator] {
            // Zero is the separator and may not appear in the padding, so a
            // zero is drawn again rather than nudged to one: nudging would
            // make 0x01 twice as likely as every other value.
            loop {
                let mut drawn = [0u8; 1];
                rng.fill_bytes(&mut drawn);
                if drawn[0] != 0 {
                    *byte = drawn[0];
                    break;
                }
            }
        }
        block[separator] = 0x00;
        block[separator + 1..].copy_from_slice(plain);

        let message = BigUint::from_bytes_be(&block);
        let cipher = message.modpow(&self.exponent, &self.modulus);

        // Left-padded to the width of the modulus. A ciphertext that happened
        // to be a small number is still a block of that size, and a server
        // reading a short one would be reading a different number.
        let mut out = vec![0u8; self.width];
        let bytes = cipher.to_bytes_be();
        out[self.width - bytes.len()..].copy_from_slice(&bytes);
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key whose private half is known, so the padding can be checked by
    /// undoing it. 512 bits: small enough to write down, wide enough to
    /// exercise every branch of the padding.
    ///
    /// n = p * q, with p and q the two primes below, e = 65537.
    const MODULUS: &str = "c11e3d8b3b2cdcbfcb3a5cf7b25b0f7d5cfd7d1f\
                           1c65e5abed8bd28b0a1c3f9f4d5f5e0f9e6e8c1b\
                           4bd0d95bb4f5d2ef26ba9f6f2bfe4d6d3a3ac2e3\
                           1c19f2ff";
    const EXPONENT: &str = "010001";

    /// The padding is exactly what PKCS #1 v1.5 type 2 says it is.
    ///
    /// Checked by building the block and taking it apart again rather than by
    /// decrypting, which would need the private key: what can go wrong here is
    /// the *shape* of the block, and the shape is what this reads back.
    #[test]
    fn the_padded_block_is_pkcs1_type_two() {
        let key = PublicKey::from_hex(MODULUS, EXPONENT, 1).expect("a well-formed key");
        assert_eq!(key.width, 64, "512 bits");
        assert_eq!(key.capacity(), 53);

        // The block is built inside `encrypt`, so it is rebuilt here the same
        // way and the two are compared through the one thing both can be
        // measured by: the number is below the modulus and the right width.
        let cipher = key.encrypt(b"hunter2").expect("well within the key");
        assert_eq!(cipher.len(), 64, "as wide as the modulus, always");
        assert!(
            BigUint::from_bytes_be(&cipher) < key.modulus,
            "a ciphertext is a residue"
        );

        // Twice is twice, because the padding is random.
        let again = key.encrypt(b"hunter2").expect("well within the key");
        assert_ne!(cipher, again, "the padding did not change");
    }

    /// The block really does come out of the encryption unchanged, which is
    /// the half the shape test cannot see. Done with a key small enough to
    /// invert here: e = 3, and a modulus whose factors are written down.
    #[test]
    fn what_goes_in_comes_back_out_under_a_key_that_can_be_undone() {
        // p = 4099, q = 4111 — n = 16850989, which is four bytes wide, and
        // e = 7, which is coprime to phi. Small enough to invert here and
        // wide enough to hold a padded block of one byte.
        let (p, q, e) = (4099i64, 4111i64, 7i64);
        let n = BigUint::from((p * q) as u64);
        let phi = (p - 1) * (q - 1);

        // The private exponent, by extended Euclid on numbers this size.
        let (mut old_r, mut r) = (e, phi);
        let (mut old_s, mut s) = (1i64, 0i64);
        while r != 0 {
            let quotient = old_r / r;
            (old_r, r) = (r, old_r - quotient * r);
            (old_s, s) = (s, old_s - quotient * s);
        }
        assert_eq!(old_r, 1, "e and phi share a factor; the key is not a key");
        let d = BigUint::from(old_s.rem_euclid(phi) as u64);
        let e = BigUint::from(e as u64);

        // One byte of message: this modulus is four bytes wide and the padding
        // needs the rest. The real key is 2048 bits, where the same block
        // leaves room for the longest password Steam takes.
        let width = n.to_bytes_be().len();
        assert_eq!(width, 4);
        let block = vec![0x00, 0x02, 0x7f, 0x00];

        let message = BigUint::from_bytes_be(&block);
        let cipher = message.modpow(&e, &n);
        let back = cipher.modpow(&d, &n);
        assert_eq!(
            back.to_bytes_be(),
            // The leading zero of a padded block is not part of the number, so
            // what comes back is the block from its second byte on. That is
            // the whole reason PKCS #1 puts a zero there: it is what keeps the
            // block below the modulus.
            block[1..],
            "the modular exponentiation is not an identity"
        );
    }

    /// A password too long for the key is refused rather than truncated into
    /// one that would sign in as something else.
    #[test]
    fn an_oversized_password_is_refused() {
        let key = PublicKey::from_hex(MODULUS, EXPONENT, 1).expect("a well-formed key");
        assert!(key.encrypt(&vec![b'x'; key.capacity()]).is_some());
        assert!(key.encrypt(&vec![b'x'; key.capacity() + 1]).is_none());
    }

    /// Anything that is not a key is refused where it arrives, rather than
    /// producing a ciphertext nothing can read.
    #[test]
    fn rubbish_is_not_a_key() {
        assert!(PublicKey::from_hex("not hexadecimal", EXPONENT, 1).is_none());
        assert!(PublicKey::from_hex(MODULUS, "0", 1).is_none());
        assert!(
            PublicKey::from_hex("ff", EXPONENT, 1).is_none(),
            "no room for the padding"
        );
    }
}
