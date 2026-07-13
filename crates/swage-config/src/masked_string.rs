use std::fmt;
use std::ops::Deref;

use serde::{Deserialize, Serialize};

/// A string whose contents are deliberately hidden from debug output.
///
/// This is a defensive logging boundary, not secret storage: callers that
/// need the value for an outbound request can access it explicitly through
/// [`expose_secret`](Self::expose_secret).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MaskedString(String);

impl MaskedString {
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for MaskedString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

impl Deref for MaskedString {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.expose_secret()
    }
}

impl From<String> for MaskedString {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for MaskedString {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_masks_the_value() {
        let secret = MaskedString::from("credential-that-must-not-appear");

        let debug = format!("{secret:?}");

        assert_eq!(debug, "[REDACTED]");
        assert!(!debug.contains(secret.expose_secret()));
    }

    #[test]
    fn serde_preserves_the_wire_value() {
        let secret = MaskedString::from("credential");
        let encoded = serde_json::to_string(&secret).expect("masked string serialises");
        let decoded: MaskedString =
            serde_json::from_str(&encoded).expect("masked string deserialises");

        assert_eq!(encoded, r#""credential""#);
        assert_eq!(decoded.expose_secret(), "credential");
    }
}
