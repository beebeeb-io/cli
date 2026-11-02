//! Typed server ids for the API layer.
//!
//! The API methods take these so a file id cannot be passed where a folder id
//! is expected. The boundary stays ergonomic: `From<&str>`/`From<String>` let
//! call sites pass the id they already hold, and `parse` is the checked
//! constructor for input that came from outside (an empty id is refused).

use super::ApiError;

macro_rules! id_type {
    ($name:ident, $what:literal) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash)]
        #[allow(dead_code)] // the API surface is intentionally wider than today's callers
        pub struct $name(String);

        #[allow(dead_code)]
        impl $name {
            /// Parse a server id, refusing an empty or whitespace-only value.
            pub fn parse(value: &str) -> Result<Self, ApiError> {
                let value = value.trim();
                if value.is_empty() {
                    return Err(ApiError::from(concat!("empty ", $what, " id")));
                }
                Ok(Self(value.to_string()))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_string())
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&String> for $name {
            fn from(value: &String) -> Self {
                Self(value.clone())
            }
        }
    };
}

id_type!(FileId, "file");
id_type!(FolderId, "folder");
id_type!(VersionId, "version");
id_type!(ShareId, "share");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_refuses_an_empty_id() {
        assert!(FileId::parse("").is_err());
        assert!(FolderId::parse("   ").is_err());
        assert!(VersionId::parse("").is_err());
        assert!(ShareId::parse("").is_err());
    }

    #[test]
    fn parse_keeps_a_real_id() {
        assert_eq!(FileId::parse(" file-1 ").unwrap().as_str(), "file-1");
        assert_eq!(ShareId::from("s1").to_string(), "s1");
    }
}
