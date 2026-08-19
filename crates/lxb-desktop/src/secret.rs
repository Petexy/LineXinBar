//! A password, from the key that types it to the pipe that consumes it.
//!
//! Two things in this shell ask for one: removing an application, which hands
//! it to `sudo` (see [`crate::uninstall`]), and an authorisation this session
//! has been asked to prove, which hands it to polkit's PAM helper (see
//! [`crate::polkit`]). Both want exactly the same care, so the care lives here
//! rather than twice.
//!
//! ## What is guaranteed, and what is not
//!
//! Guaranteed: the bytes live in one allocation that never moves, they are
//! overwritten when the [`Secret`] is dropped, and nothing prints them — the
//! [`std::fmt::Debug`] below is there so that a stray `?secret` in a `tracing`
//! call cannot leak one. The panel that collects it is drawn from a *count* of
//! characters, so not even the layout holds a copy.
//!
//! Not guaranteed: the shell is not `mlock`ed, so a machine that swaps under
//! memory pressure could write a page out, and the kernel could hold a copy of
//! whatever pipe or socket it was handed to. Fixing those means locking pages
//! and is a change to how the whole process starts, not something this type can
//! do on its own.

use std::io::Write;
use std::sync::atomic::{compiler_fence, Ordering};

/// The longest password the field will take, in bytes.
///
/// Not a policy about passwords — it is what lets the buffer be allocated once
/// and never grow. A `Vec` that reallocates leaves the old contents lying in
/// the allocator, and there is no getting them back to overwrite.
const CAPACITY: usize = 256;

/// A password, held for as long as it takes to hand over.
pub struct Secret {
    bytes: Vec<u8>,
    /// How many characters have been typed, which is how many marks the field
    /// draws. Counted rather than derived, because it is asked for every frame
    /// and a UTF-8 walk per frame to draw dots would be silly.
    typed: usize,
}

impl Default for Secret {
    fn default() -> Self {
        Self {
            bytes: Vec::with_capacity(CAPACITY),
            typed: 0,
        }
    }
}

impl Secret {
    /// Add a character. Refused, silently, once the buffer is full: growing it
    /// would be the one thing this type exists to avoid.
    pub fn push(&mut self, character: char) {
        let mut encoded = [0u8; 4];
        let encoded = character.encode_utf8(&mut encoded).as_bytes();
        if self.bytes.len() + encoded.len() > CAPACITY {
            return;
        }
        self.bytes.extend_from_slice(encoded);
        self.typed += 1;
    }

    /// Take the last character off, whatever it was made of.
    pub fn pop(&mut self) {
        // Back over the continuation bytes to the head of the last character.
        while let Some(&last) = self.bytes.last() {
            self.bytes.pop();
            if last & 0b1100_0000 != 0b1000_0000 {
                break;
            }
        }
        self.typed = self.typed.saturating_sub(1);
    }

    /// How many characters are in it, for the marks the field draws.
    pub fn typed(&self) -> usize {
        self.typed
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Write it down `sink` as one line, and say whether that worked.
    ///
    /// The way out for the two that hand a password to a *program*: both write
    /// it to the other end of a pipe or a socket, both terminate it with a
    /// newline, and neither has any use for a `String` it would then have to
    /// remember to overwrite.
    pub fn hand_to(&self, sink: &mut impl Write) -> std::io::Result<()> {
        sink.write_all(&self.bytes)?;
        sink.write_all(b"\n")?;
        sink.flush()
    }

    /// Show it to one closure as text, and give back whatever that said.
    ///
    /// The other way out, for the one caller that is not writing to a pipe: a
    /// wireless password goes into a D-Bus message as a string, because a
    /// pre-shared key is the one shape `NetworkManager` will take it in. See
    /// [`crate::network`].
    ///
    /// A closure rather than an accessor, and that is the whole of the care
    /// this can still take: the borrow ends when the closure returns, so the
    /// only text there ever is points into the same buffer [`Drop`] overwrites,
    /// and nothing can hold it afterwards. What the caller must not do — and
    /// the reason this is not `as_str` — is copy it into a `String` inside the
    /// closure; the whole message is built in there instead.
    ///
    /// `None` if the bytes are not text, which nothing that went through
    /// [`Secret::push`] can be: characters are encoded whole and
    /// [`Secret::pop`] takes them off whole. It is checked rather than assumed
    /// because a type that looks after a password should not be the one place
    /// in the shell that trusts its own invariants without saying so.
    pub fn as_text<T>(&self, with: impl FnOnce(&str) -> T) -> Option<T> {
        Some(with(std::str::from_utf8(&self.bytes).ok()?))
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.bytes.fill(0);
        // The write above is dead as far as the optimiser is concerned — the
        // buffer is about to be freed. This is the usual way of asking for it
        // to happen anyway without reaching for `unsafe`.
        compiler_fence(Ordering::SeqCst);
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secret({} characters)", self.typed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The buffer is allocated once and never grows, so there is only ever one
    /// copy of the password to overwrite.
    #[test]
    fn a_secret_never_reallocates_and_counts_characters_not_bytes() {
        let mut secret = Secret::default();
        assert!(secret.is_empty());
        let start = secret.bytes.as_ptr();

        for character in "pa55wörd🔑".chars() {
            secret.push(character);
        }
        assert_eq!(secret.typed(), 9, "characters, not bytes");
        assert!(secret.bytes.len() > 9, "and it really is multi-byte");
        assert_eq!(secret.bytes.as_ptr(), start, "the buffer moved");

        // Backspace takes a whole character off, however wide it was.
        secret.pop();
        assert_eq!(secret.typed(), 8);
        assert_eq!(secret.bytes.len(), "pa55wörd".len());
        assert_eq!(
            std::str::from_utf8(&secret.bytes),
            Ok("pa55wörd"),
            "a partial character was left behind"
        );
    }

    /// A full buffer stops taking characters rather than growing.
    #[test]
    fn a_secret_stops_rather_than_growing() {
        let mut secret = Secret::default();
        let start = secret.bytes.as_ptr();
        for _ in 0..CAPACITY * 2 {
            secret.push('x');
        }
        assert_eq!(secret.bytes.len(), CAPACITY);
        assert_eq!(secret.bytes.as_ptr(), start);
    }

    /// It must not be printable, because the whole shell logs with `tracing`
    /// and one `?password` would be enough.
    #[test]
    fn a_secret_does_not_print_itself() {
        let mut secret = Secret::default();
        for character in "hunter2".chars() {
            secret.push(character);
        }
        let shown = format!("{secret:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert_eq!(shown, "Secret(7 characters)");
    }

    /// Handed over as one line, ending in the newline both readers wait for and
    /// carrying nothing else — an empty password is a bare newline rather than
    /// nothing at all, or the program on the other end would sit waiting for a
    /// line that never came.
    #[test]
    fn a_secret_is_handed_over_as_one_line() {
        let mut secret = Secret::default();
        for character in "pa55wörd".chars() {
            secret.push(character);
        }
        let mut sink = Vec::new();
        secret.hand_to(&mut sink).expect("a Vec cannot fail");
        assert_eq!(sink, b"pa55w\xc3\xb6rd\n");

        let mut sink = Vec::new();
        Secret::default().hand_to(&mut sink).expect("nor can this");
        assert_eq!(sink, b"\n");
    }
}
