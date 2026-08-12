//! A password on its way from the field to the key that encrypts it.
//!
//! The shell has a type for this already — the one the uninstall panel and the
//! polkit panel fill in — and its only way out is to write itself down a sink
//! as one line. That is deliberate there: both of those hand the password to a
//! program on the other end of a pipe, so a sink is the whole of what they
//! need and an accessor for the bytes would be a way for a copy to escape.
//!
//! Signing in to Steam wants the same care and a different ending: the
//! password is encrypted here, in this process, under a key Steam sends for
//! that account, and what leaves is the ciphertext. So this is the sink the
//! shell's own field writes into — [`std::io::Write`], which is exactly what
//! that field already knows how to hand itself to — and the bytes are readable
//! only inside this crate, only by the encryption, and are overwritten when it
//! is done.
//!
//! What is *not* guaranteed is the same as there: no page is locked, so a
//! machine that swaps under memory pressure could write one out.

use std::io::Write;

/// The longest password this will take, in bytes.
///
/// The same figure the shell's own field uses, and for the same reason: the
/// buffer is allocated once and never grows, because a `Vec` that reallocates
/// leaves the old contents in the allocator where nothing can overwrite them.
const CAPACITY: usize = 256;

/// A password, held for as long as it takes to encrypt it.
pub struct Password {
    bytes: Vec<u8>,
}

impl Default for Password {
    fn default() -> Self {
        Password {
            bytes: Vec::with_capacity(CAPACITY),
        }
    }
}

impl Password {
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// What the encryption reads. Crate-private on purpose: this is the one
    /// place the plaintext is looked at, and it is looked at to stop being
    /// plaintext.
    pub(crate) fn plain(&self) -> &[u8] {
        &self.bytes
    }
}

/// The one way in, which is the one way the shell's own field can hand itself
/// over — see [`crate::Steam::sign_in_with_password`].
///
/// A field that arrives ending in a newline has it taken off: that field
/// writes itself as one *line*, because the two things it was built for read
/// lines, and a trailing newline encrypted into the ciphertext would be a
/// character of a password the user never typed.
impl Write for Password {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let room = CAPACITY.saturating_sub(self.bytes.len());
        let taken = buf.len().min(room);
        self.bytes.extend_from_slice(&buf[..taken]);
        while self.bytes.last() == Some(&b'\n') || self.bytes.last() == Some(&b'\r') {
            self.bytes.pop();
        }
        // The whole write is reported as done even when the buffer was full:
        // refusing would make the caller loop, and the field on the other end
        // has its own limit at the same figure. A password longer than this is
        // refused where it is encrypted, not silently truncated into one that
        // might sign in as something else — see `Password::plain`'s only
        // caller.
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for Password {
    fn drop(&mut self) {
        self.bytes.fill(0);
        // The write above is dead as far as the optimiser is concerned — the
        // buffer is about to be freed. This is the usual way of asking for it
        // to happen anyway without reaching for `unsafe`.
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}

/// Never printable, for the reason the shell's own field is not: one `?field`
/// in a `tracing` call would be enough.
impl std::fmt::Debug for Password {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Password({} bytes)", self.bytes.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shell's field writes itself as one line; the line ending is not
    /// part of the password.
    #[test]
    fn the_trailing_newline_of_a_handed_over_field_is_not_a_character() {
        let mut password = Password::default();
        password.write_all(b"pa55w\xc3\xb6rd\n").expect("in memory");
        assert_eq!(password.plain(), "pa55wörd".as_bytes());
        assert!(!password.is_empty());
    }

    /// It arrives in whatever pieces the writer chose, which for a field
    /// handing itself over is the password and then the newline.
    #[test]
    fn it_can_be_written_in_pieces() {
        let mut password = Password::default();
        password.write_all(b"hunter").expect("in memory");
        password.write_all(b"2").expect("in memory");
        password.write_all(b"\n").expect("in memory");
        assert_eq!(password.plain(), b"hunter2");
    }

    /// A full buffer stops taking bytes rather than growing, so there is only
    /// ever one copy to overwrite.
    #[test]
    fn it_stops_rather_than_growing() {
        let mut password = Password::default();
        let start = password.bytes.as_ptr();
        for _ in 0..CAPACITY * 2 {
            password.write_all(b"x").expect("in memory");
        }
        assert_eq!(password.bytes.len(), CAPACITY);
        assert_eq!(password.bytes.as_ptr(), start, "the buffer moved");
    }

    /// It must not print itself, because this crate logs with `tracing`.
    #[test]
    fn it_does_not_print_itself() {
        let mut password = Password::default();
        password.write_all(b"hunter2").expect("in memory");
        let shown = format!("{password:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert_eq!(shown, "Password(7 bytes)");
    }
}
