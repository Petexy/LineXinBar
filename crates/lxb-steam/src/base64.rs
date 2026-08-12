//! Base 64, in the two spellings Steam uses.
//!
//! The request body carries a protobuf message as standard base 64, and the
//! token that comes back is a JSON Web Token, whose three parts are base 64
//! with a different last two characters and no padding. Both are RFC 4648, and
//! the whole of the difference between them is that alphabet.
//!
//! Written here rather than taken as a dependency for the reason the `.desktop`
//! parser in the shell is: this is sixty lines of a frozen specification, and
//! the two of them are the only encodings this crate does not get from a
//! protocol library it already has.

const STANDARD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL_SAFE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Encode with the standard alphabet and `=` padding — what the body of a
/// request to Steam's services is.
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let block = (u32::from(chunk[0]) << 16)
            | (u32::from(chunk.get(1).copied().unwrap_or(0)) << 8)
            | u32::from(chunk.get(2).copied().unwrap_or(0));
        for step in 0..4 {
            // A chunk of one byte says two characters and a chunk of two says
            // three; the rest is padding, which is what makes the length of
            // the encoding say how long the input was.
            if step <= chunk.len() {
                let index = (block >> (18 - step * 6)) & 0x3f;
                out.push(STANDARD[index as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Decode either alphabet, with or without padding.
///
/// One function for both because a decoder that insisted on knowing which it
/// was being handed would have to be told by every caller, and the two
/// alphabets do not overlap: `-` and `_` mean what `+` and `/` mean, and
/// nothing means both.
///
/// `None` for anything that is not base 64 at all — a character outside both
/// alphabets, or a length no encoding could have produced.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut block = 0u32;
    let mut have = 0u32;

    for character in text.bytes() {
        if character == b'=' || character == b'\r' || character == b'\n' {
            continue;
        }
        let value = STANDARD
            .iter()
            .position(|&c| c == character)
            .or_else(|| URL_SAFE.iter().position(|&c| c == character))?;
        block = (block << 6) | value as u32;
        have += 6;
        if have >= 8 {
            have -= 8;
            out.push((block >> have) as u8);
        }
    }

    // What is left over must be the padding bits of the last group, which are
    // zero. Anything else is a truncated encoding rather than a short one.
    if block & ((1 << have) - 1) != 0 {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vectors from RFC 4648, which are the whole specification of this.
    #[test]
    fn the_rfc_vectors_encode_and_decode() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(encode(plain.as_bytes()), encoded, "encoding {plain:?}");
            assert_eq!(
                decode(encoded).as_deref(),
                Some(plain.as_bytes()),
                "decoding {encoded:?}"
            );
        }
    }

    /// Both alphabets decode, which is what lets one function read a request
    /// body and the middle of a token.
    #[test]
    fn both_alphabets_read_back() {
        let awkward = [0xfb, 0xff, 0xbe];
        assert_eq!(encode(&awkward), "+/++");
        assert_eq!(decode("+/++").as_deref(), Some(&awkward[..]));
        assert_eq!(decode("-_--").as_deref(), Some(&awkward[..]));
    }

    /// A token's parts arrive without padding, so a decoder that required it
    /// would refuse every one of them.
    #[test]
    fn padding_is_optional_on_the_way_in() {
        assert_eq!(decode("Zm9vYmE").as_deref(), Some(&b"fooba"[..]));
        assert_eq!(decode("Zg").as_deref(), Some(&b"f"[..]));
    }

    /// Anything that is not base 64 is refused rather than half-read.
    #[test]
    fn rubbish_is_refused() {
        assert_eq!(decode("Zm9v*mFy"), None, "a character in neither alphabet");
        assert_eq!(decode("Zh"), None, "bits set outside the last byte");
    }
}
