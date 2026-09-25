//! A token, held for as long as it is needed and cleared when it goes.
//!
//! The same care the RetroArch helper takes with a RetroAchievements password,
//! and with the same honesty about its limits: the bytes this process owns are
//! overwritten with zeroes when they are dropped, behind a compiler fence so
//! the write is not optimised away. What HTTP and JSON libraries copied on the
//! way through is not claimed to be cleared.

use serde_json::Value;

/// A string that is never printed and is wiped on drop.
pub struct Secret(String);

impl Secret {
    pub fn new(text: String) -> Secret {
        Secret(text)
    }

    /// The text itself, for the one place it has to go.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(…)")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        // SAFETY: zeroes are valid UTF-8, so the string stays a string.
        unsafe { self.0.as_bytes_mut().fill(0) };
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}

/// Clear every string in a parsed answer.
pub fn wipe(value: &mut Value) {
    match value {
        // SAFETY: as above.
        Value::String(text) => unsafe { text.as_bytes_mut().fill(0) },
        Value::Array(items) => items.iter_mut().for_each(wipe),
        Value::Object(fields) => fields.values_mut().for_each(wipe),
        _ => {}
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_prints_as_nothing() {
        assert_eq!(format!("{:?}", Secret::new("hunter2".into())), "Secret(…)");
    }

    #[test]
    fn wiping_clears_every_string_and_keeps_the_shape() {
        let mut value = serde_json::json!({"a": "token", "b": ["x", {"c": "y"}], "n": 3});
        wipe(&mut value);
        assert_eq!(value["a"], "\0\0\0\0\0");
        assert_eq!(value["b"][0], "\0");
        assert_eq!(value["b"][1]["c"], "\0");
        assert_eq!(value["n"], 3);
    }
}
