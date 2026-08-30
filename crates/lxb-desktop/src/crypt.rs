//! One password, in the shape `/etc/shadow` keeps them: SHA-512 crypt.
//!
//! ## Why this is here rather than linked
//!
//! `accounts-daemon` will not take a password. It takes a *hash* —
//! `SetPassword` is handed the string that goes into `/etc/shadow`, and the
//! hashing happens wherever the password was collected, which is here. That is
//! not an accident of the interface: it is what keeps the plaintext from
//! crossing a bus, being logged by anything on the way, or sitting in a
//! daemon's address space. What leaves this shell is a hash and a salt.
//!
//! Every desktop that offers this reaches for `crypt(3)` out of libxcrypt.
//! This does not, for the reason `lxb-steam` writes its own modular
//! exponentiation rather than linking OpenSSL: the shell is one binary for a
//! machine that may have nothing on it, and a link against `libcrypt.so` is a
//! build that fails on a machine without the development half of a library
//! this needs one function from. There is a second reason and it is the better
//! one — `crypt(3)` cannot be *tested*. It returns a pointer into static
//! storage, it is not thread-safe, its thread-safe form takes a 32-kilobyte
//! struct whose layout differs between glibc and libxcrypt, and what it
//! computes can only be checked against itself. What is written here is checked
//! against the published vectors, in this crate's own test suite, on every
//! build.
//!
//! ## What it computes
//!
//! `$6$`, which is Ulrich Drepper's SHA-512 scheme — the one nearly every Linux
//! machine's `/etc/shadow` is written in, and the one every PAM stack in
//! existence reads. Deliberately not yescrypt, which some distributions now
//! default to: yescrypt is the better function, and it is also newer than a
//! great many of the machines this shell is for. A `$6$` hash is understood by
//! all of them, including the ones that write yescrypt themselves — the prefix
//! says which scheme a line is in, so a `$6$` password set here works on a
//! machine whose other accounts are yescrypt.
//!
//! The rounds are left at the scheme's default of 5000 and not written into the
//! setting, which is what makes the output byte-for-byte what `mkpasswd -m
//! sha512crypt` produces for the same salt.

use std::fmt::Write as _;

/// The alphabet a crypt hash is written in — not RFC 4648's, and not in its
/// order either.
const ALPHABET: &[u8; 64] = b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// How many characters of salt. Sixteen is the scheme's maximum and what every
/// tool that writes one uses: the salt's whole job is to make two accounts with
/// the same password hash differently, and there is no reason to take less of
/// it than the format holds.
const SALT_LENGTH: usize = 16;

/// The default number of rounds, which is what a setting with no `rounds=` in
/// it means.
const ROUNDS: usize = 5000;

/// What the scheme will take, either side of that. A `rounds=` outside this is
/// pulled to the nearest end rather than refused, which is what every
/// implementation of it does.
const ROUNDS_MIN: usize = 1_000;
const ROUNDS_MAX: usize = 999_999_999;

/// Hash `password` with a fresh random salt, ready for `/etc/shadow`.
///
/// `None` only when the machine will not give up sixteen random bytes, which is
/// a machine with no `/dev/urandom` — and a shell that made up a salt at that
/// point would be writing a password the whole world can precompute. Better to
/// fail the press and say so.
pub fn hash(password: &str) -> Option<String> {
    Some(hash_with(password, &salt()?, None))
}

/// The same, against a salt somebody else chose and a round count that may not
/// be the default. The whole of what the tests drive, and what makes the
/// published vectors checkable.
///
/// `rounds` is `None` for the scheme's own 5000, which is what this shell always
/// asks for and what a setting with no `rounds=` in it means. It is a parameter
/// at all because three of the five published vectors name a count — so without
/// it, three fifths of the only external check on this code could not be run.
fn hash_with(password: &str, salt: &str, rounds: Option<usize>) -> String {
    let password = password.as_bytes();
    // The scheme takes at most sixteen characters of salt and ignores the rest,
    // which is what the second published vector is about.
    let salt = &salt.as_bytes()[..salt.len().min(SALT_LENGTH)];

    // B, the digest of "password salt password" — the value everything below is
    // built out of.
    let mut b = Sha512::new();
    b.write(password);
    b.write(salt);
    b.write(password);
    let b = b.finish();

    // A, the digest the rounds start from.
    let mut a = Sha512::new();
    a.write(password);
    a.write(salt);
    // As many bytes of B as the password is long, B repeated where it is
    // shorter than the password.
    a.write(&repeated(&b, password.len()));
    // Then the password's length in binary, low bit first: a set bit takes a
    // whole B and a clear one takes the password. There is no reason for this
    // beyond the specification saying so, and it is the step every
    // reimplementation gets wrong.
    let mut count = password.len();
    while count > 0 {
        if count & 1 == 1 {
            a.write(&b);
        } else {
            a.write(password);
        }
        count >>= 1;
    }
    let mut a = a.finish();

    // P, the password's own sequence: the digest of the password written out as
    // many times as it is long, repeated to the password's length.
    let mut dp = Sha512::new();
    for _ in 0..password.len() {
        dp.write(password);
    }
    let p = repeated(&dp.finish(), password.len());

    // S, the salt's, on the same terms — except that how many times the salt is
    // written depends on the first byte of A, which is what stops the two
    // sequences from being computable in parallel with the rounds.
    let mut ds = Sha512::new();
    for _ in 0..(16 + usize::from(a[0])) {
        ds.write(salt);
    }
    let s = repeated(&ds.finish(), salt.len());

    // And the rounds themselves: five thousand digests, each over the last, so
    // that checking one password costs five thousand times what checking a
    // digest would. That is the whole of what the scheme is for.
    let count = rounds.map_or(ROUNDS, |rounds| rounds.clamp(ROUNDS_MIN, ROUNDS_MAX));
    for round in 0..count {
        let mut c = Sha512::new();
        if round & 1 == 1 {
            c.write(&p);
        } else {
            c.write(&a);
        }
        if round % 3 != 0 {
            c.write(&s);
        }
        if round % 7 != 0 {
            c.write(&p);
        }
        if round & 1 == 1 {
            c.write(&a);
        } else {
            c.write(&p);
        }
        a = c.finish();
    }

    let mut out = String::with_capacity(120);
    out.push_str("$6$");
    // The count is written down only when it was asked for. A setting that
    // spelled out the default would hash the same and read differently from
    // every other line in the file, and `mkpasswd` does not write one either.
    if let Some(rounds) = rounds {
        let _ = write!(out, "rounds={}$", rounds.clamp(ROUNDS_MIN, ROUNDS_MAX));
    }
    out.push_str(std::str::from_utf8(salt).unwrap_or_default());
    out.push('$');
    out.push_str(&encode(&a));
    out
}

/// `source`, repeated or cut to exactly `wanted` bytes.
fn repeated(source: &[u8; 64], wanted: usize) -> Vec<u8> {
    source.iter().copied().cycle().take(wanted).collect()
}

/// The hash, in the order and the alphabet a crypt string is written in.
///
/// Neither is the obvious one. The bytes are taken three at a time in a
/// permutation the specification lists out — the table below is that list — and
/// each group of three is written low six bits first, which is the reverse of
/// how base64 is usually read.
fn encode(hash: &[u8; 64]) -> String {
    const ORDER: [[usize; 3]; 21] = [
        [0, 21, 42],
        [22, 43, 1],
        [44, 2, 23],
        [3, 24, 45],
        [25, 46, 4],
        [47, 5, 26],
        [6, 27, 48],
        [28, 49, 7],
        [50, 8, 29],
        [9, 30, 51],
        [31, 52, 10],
        [53, 11, 32],
        [12, 33, 54],
        [34, 55, 13],
        [56, 14, 35],
        [15, 36, 57],
        [37, 58, 16],
        [59, 17, 38],
        [18, 39, 60],
        [40, 61, 19],
        [62, 20, 41],
    ];
    let mut out = String::with_capacity(86);
    for [high, middle, low] in ORDER {
        group(
            &mut out,
            u32::from(hash[high]) << 16 | u32::from(hash[middle]) << 8 | u32::from(hash[low]),
            4,
        );
    }
    // The sixty-fourth byte on its own, as two characters.
    group(&mut out, u32::from(hash[63]), 2);
    out
}

fn group(out: &mut String, mut word: u32, characters: usize) {
    for _ in 0..characters {
        out.push(char::from(ALPHABET[(word & 0x3f) as usize]));
        word >>= 6;
    }
}

/// Sixteen characters of salt, from the kernel's own pool.
fn salt() -> Option<String> {
    // A bounded read, and it has to be: `/dev/urandom` never reaches the end,
    // so anything that reads it whole — `std::fs::read`, `read_to_end` — grows
    // its buffer until the machine is out of memory. Sixteen bytes, exactly.
    use std::io::Read;
    let mut file = std::fs::File::open("/dev/urandom").ok()?;
    let mut bytes = [0u8; SALT_LENGTH];
    file.read_exact(&mut bytes).ok()?;
    let mut salt = String::with_capacity(SALT_LENGTH);
    for byte in bytes {
        // Folded into the alphabet rather than rejected and drawn again: the
        // salt is not a secret and needs no uniformity, only difference.
        let _ = write!(salt, "{}", char::from(ALPHABET[usize::from(byte) & 0x3f]));
    }
    Some(salt)
}

// --- SHA-512 ---------------------------------------------------------------

/// FIPS 180-4's SHA-512, as much of it as one password needs.
///
/// Written out rather than depended on for the reason the scheme above is: this
/// is the only digest in the shell, it is a hundred lines, and what it computes
/// is checked against the standard's own vectors below.
struct Sha512 {
    state: [u64; 8],
    /// What has not yet made a whole block.
    tail: Vec<u8>,
    /// How many bytes have been written in total, which the padding ends with.
    written: u128,
}

#[rustfmt::skip]
const K: [u64; 80] = [
    0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc,
    0x3956c25bf348b538, 0x59f111f1b605d019, 0x923f82a4af194f9b, 0xab1c5ed5da6d8118,
    0xd807aa98a3030242, 0x12835b0145706fbe, 0x243185be4ee4b28c, 0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f, 0x80deb1fe3b1696b1, 0x9bdc06a725c71235, 0xc19bf174cf692694,
    0xe49b69c19ef14ad2, 0xefbe4786384f25e3, 0x0fc19dc68b8cd5b5, 0x240ca1cc77ac9c65,
    0x2de92c6f592b0275, 0x4a7484aa6ea6e483, 0x5cb0a9dcbd41fbd4, 0x76f988da831153b5,
    0x983e5152ee66dfab, 0xa831c66d2db43210, 0xb00327c898fb213f, 0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2, 0xd5a79147930aa725, 0x06ca6351e003826f, 0x142929670a0e6e70,
    0x27b70a8546d22ffc, 0x2e1b21385c26c926, 0x4d2c6dfc5ac42aed, 0x53380d139d95b3df,
    0x650a73548baf63de, 0x766a0abb3c77b2a8, 0x81c2c92e47edaee6, 0x92722c851482353b,
    0xa2bfe8a14cf10364, 0xa81a664bbc423001, 0xc24b8b70d0f89791, 0xc76c51a30654be30,
    0xd192e819d6ef5218, 0xd69906245565a910, 0xf40e35855771202a, 0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8, 0x1e376c085141ab53, 0x2748774cdf8eeb99, 0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63, 0x4ed8aa4ae3418acb, 0x5b9cca4f7763e373, 0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc, 0x78a5636f43172f60, 0x84c87814a1f0ab72, 0x8cc702081a6439ec,
    0x90befffa23631e28, 0xa4506cebde82bde9, 0xbef9a3f7b2c67915, 0xc67178f2e372532b,
    0xca273eceea26619c, 0xd186b8c721c0c207, 0xeada7dd6cde0eb1e, 0xf57d4f7fee6ed178,
    0x06f067aa72176fba, 0x0a637dc5a2c898a6, 0x113f9804bef90dae, 0x1b710b35131c471b,
    0x28db77f523047d84, 0x32caab7b40c72493, 0x3c9ebe0a15c9bebc, 0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6, 0x597f299cfc657e2a, 0x5fcb6fab3ad6faec, 0x6c44198c4a475817,
];

impl Sha512 {
    fn new() -> Self {
        Self {
            state: [
                0x6a09e667f3bcc908,
                0xbb67ae8584caa73b,
                0x3c6ef372fe94f82b,
                0xa54ff53a5f1d36f1,
                0x510e527fade682d1,
                0x9b05688c2b3e6c1f,
                0x1f83d9abfb41bd6b,
                0x5be0cd19137e2179,
            ],
            tail: Vec::with_capacity(128),
            written: 0,
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        self.written += bytes.len() as u128;
        self.tail.extend_from_slice(bytes);
        while self.tail.len() >= 128 {
            let block: [u8; 128] = self.tail[..128].try_into().expect("128 bytes");
            self.compress(&block);
            self.tail.drain(..128);
        }
    }

    fn finish(mut self) -> [u8; 64] {
        // A one bit, then zeros, then the length in bits as a 128-bit number —
        // and the whole thing has to land on a block boundary.
        let bits = self.written * 8;
        self.tail.push(0x80);
        while self.tail.len() % 128 != 112 {
            self.tail.push(0);
        }
        let tail = std::mem::take(&mut self.tail);
        self.tail = tail;
        self.tail.extend_from_slice(&bits.to_be_bytes());
        while self.tail.len() >= 128 {
            let block: [u8; 128] = self.tail[..128].try_into().expect("128 bytes");
            self.compress(&block);
            self.tail.drain(..128);
        }
        let mut out = [0u8; 64];
        for (word, chunk) in self.state.iter().zip(out.chunks_exact_mut(8)) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    fn compress(&mut self, block: &[u8; 128]) {
        let mut w = [0u64; 80];
        for (i, chunk) in block.chunks_exact(8).enumerate() {
            w[i] = u64::from_be_bytes(chunk.try_into().expect("8 bytes"));
        }
        for i in 16..80 {
            let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
            let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..80 {
            let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
            let choice = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (word, add) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *word = word.wrapping_add(add);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(message: &[u8]) -> String {
        let mut sha = Sha512::new();
        sha.write(message);
        sha.finish().iter().map(|b| format!("{b:02x}")).collect()
    }

    /// FIPS 180-4's own vectors, plus a message long enough to cross a block
    /// boundary and one long enough that its padding needs a block of its own.
    #[test]
    fn sha512_is_sha512() {
        assert_eq!(
            digest(b"abc"),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
             2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
        assert_eq!(
            digest(b""),
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce\
             47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
        );
        assert_eq!(
            digest(
                b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmn\
                  hijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu"
                    .iter()
                    .copied()
                    .filter(|b| !b.is_ascii_whitespace())
                    .collect::<Vec<u8>>()
                    .as_slice()
            ),
            "8e959b75dae313da8cf4f72814fc143f8f7779c6eb9f7fa17299aeadb6889018\
             501d289e4900f7e4331b99dec4b5433ac7d329eeb6dd26545e96e55b874be909"
        );
        // A million 'a's is the standard's fourth vector, and the one that says
        // the running state is carried between blocks correctly.
        let mut sha = Sha512::new();
        for _ in 0..1000 {
            sha.write(&[b'a'; 1000]);
        }
        let million: String = sha.finish().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            million,
            "e718483d0ce769644e2e42c7bc15b4638e1f98b13b2044285632a803afa973eb\
             de0ff244877ea60a4cb0432ce577c31beb009c5c2c49aa2e4eadb217ad8cc09b"
        );
    }

    /// The scheme's own published vectors — Drepper's `SHA-crypt.txt`. This is
    /// the whole reason the hashing is written here rather than linked: a
    /// password set by this shell has to be one PAM will accept, and nothing
    /// else in the session can say whether it is.
    #[test]
    fn sha512_crypt_matches_the_published_vectors() {
        // All five of them, in the order `SHA-crypt.txt` lists them, each also
        // checked against this machine's own libxcrypt — `perl -e 'print
        // crypt($password, $setting)'` — because a vector copied wrongly is a
        // test that pins the wrong answer, and one of these was.
        //
        // The password with no space where two lines meet is not a mistake: the
        // C in that document relies on adjacent string literals joining with
        // nothing between them, so `more` and `than` really do run together.
        assert_eq!(
            hash_with("Hello world!", "saltstring", None),
            "$6$saltstring$svn8UoSVapNtMuq1ukKS4tPQd8iKwSMHWjl/O817G3uBnIFNjnQJuesI68u4OTLiBFdcbYEdFCoEOfaS35inz1"
        );
        // A salt longer than sixteen characters is cut, and the setting written
        // back names the cut one — a hash that named the whole salt would be one
        // nothing could ever verify.
        assert_eq!(
            hash_with("Hello world!", "saltstringsaltstring", Some(10_000)),
            "$6$rounds=10000$saltstringsaltst$OW1/O6BYHV6BcXZu8QVeXbDWra3Oeqh0sbHbbMCVNSnCM/UrjmM0Dp8vOuZeHBy/YTBmSK6H9qs/y3RnOaw5v."
        );
        // Spelling out the default is the one case where the count *is* written
        // back, because the vector was taken with it written.
        assert_eq!(
            hash_with("This is just a test", "toolongsaltstring", Some(5_000)),
            "$6$rounds=5000$toolongsaltstrin$lQ8jolhgVRVhY4b5pZKaysCLi0QBxGoNeKQzQ3glMhwllF7oGDZxUhx1yxdYcz/e1JSbq3y6JMxxl8audkUEm0"
        );
        assert_eq!(
            hash_with(
                "a very much longer text to encrypt.  This one even stretches over morethan one line.",
                "anotherlongsaltstring",
                Some(1_400)
            ),
            "$6$rounds=1400$anotherlongsalts$POfYwTEok97VWcjxIiSOjiykti.o/pQs.wPvMxQ6Fm7I6IoYN3CmLs66x9t0oSwbtEW7o7UmJEiDwGqd8p4ur1"
        );
        assert_eq!(
            hash_with(
                "we have a short salt string but not a short password",
                "short",
                Some(77_777)
            ),
            "$6$rounds=77777$short$WuQyW2YR.hBNpjjRhpYD/ifIw05xdfeEyQoMxIXbkvr0gge1a1x3yRULJ5CCaUeOxFmtlcGZelFl5CxtgfiAc0"
        );
        // And the default really is 5000: the same password with the count left
        // out hashes to what naming it produces, minus the `rounds=` the setting
        // then carries.
        assert_eq!(
            hash_with("This is just a test", "toolongsaltstring", None),
            hash_with("This is just a test", "toolongsaltstring", Some(5_000))
                .replace("rounds=5000$", "")
        );
    }

    /// A fresh hash is in the shape `/etc/shadow` keeps, has sixteen characters
    /// of salt, and is different every time — which is the whole of what the
    /// salt is for.
    #[test]
    fn a_fresh_hash_is_salted_and_never_repeats() {
        let first = hash("hunter2").expect("this machine has /dev/urandom");
        let second = hash("hunter2").expect("this machine has /dev/urandom");
        assert_ne!(first, second, "the same salt was drawn twice");
        for hash in [&first, &second] {
            let mut parts = hash.split('$');
            assert_eq!(parts.next(), Some(""));
            assert_eq!(parts.next(), Some("6"));
            assert_eq!(parts.next().map(str::len), Some(SALT_LENGTH));
            assert_eq!(parts.next().map(str::len), Some(86));
            assert_eq!(parts.next(), None);
        }
        // And it is the hash of *that* password with *that* salt, which is what
        // makes it verifiable by anything but this code.
        let salt = first.split('$').nth(2).expect("a salt");
        assert_eq!(hash_with("hunter2", salt, None), first);
    }
}
