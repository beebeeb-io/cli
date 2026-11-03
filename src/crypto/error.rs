//! The one failure type for the crypto helpers.
//!
//! Owns: `CryptoError`, the four ways a read can refuse — a file id that is not
//! a UUID, metadata that carries no readable pin, a chunk stream whose shape
//! does not match the row, and bytes that authenticate under no candidate key.
//! Must not: decide which key to try (that is `chunks`/`pins`).

/// Why a crypto helper refused to read or open bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CryptoError {
    /// The file id is not a UUID.
    InvalidId(String),
    /// The encrypted metadata carries no readable chunk pin.
    Metadata(String),
    /// The chunk stream's shape does not match what the row claims.
    Malformed(String),
    /// The bytes did not authenticate under any candidate key.
    Decrypt(String),
}

impl CryptoError {
    /// The message every variant carries, for the string-shaped exit paths.
    pub fn message(&self) -> &str {
        match self {
            Self::InvalidId(m) | Self::Metadata(m) | Self::Malformed(m) | Self::Decrypt(m) => m,
        }
    }
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for CryptoError {}

impl From<CryptoError> for String {
    fn from(error: CryptoError) -> Self {
        error.message().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_variant_keeps_its_message() {
        for error in [
            CryptoError::InvalidId("id".into()),
            CryptoError::Metadata("pin".into()),
            CryptoError::Malformed("shape".into()),
            CryptoError::Decrypt("key".into()),
        ] {
            assert_eq!(error.to_string(), error.message());
            assert_eq!(String::from(error.clone()), error.message());
        }
    }

    #[test]
    fn the_variants_are_distinct() {
        assert_ne!(CryptoError::InvalidId("x".into()), CryptoError::Malformed("x".into()));
        assert_ne!(CryptoError::Metadata("x".into()), CryptoError::Decrypt("x".into()));
    }
}
