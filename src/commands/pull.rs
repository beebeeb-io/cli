//! `bb pull` — download one file (by id prefix or path) or a whole folder.
//!
//! Owns: the command entry point ([`run`]), argument resolution (id vs path),
//! overwrite confirmation, the folder walk and single-file download, and the
//! `--zip` archive writer.
//! Must not: decide what exists remotely (see `crate::api`) or how bytes are
//! decrypted (see `crate::download`).
//! Tests: `resolve::failure_tests` (version-change retry, ask-once) and
//! `zip::tests` (hostile names, collision suffixes, plaintext-byte progress).

use colored::Colorize;
use std::io::IsTerminal;
use std::path::PathBuf;

use crate::api::ApiClient;
use crate::commands::push::load_master_key;

mod error;
use error::*;

mod resolve;
use resolve::*;
pub(crate) use resolve::resolve_file_arg;

mod folder;
use folder::*;

// ---------------------------------------------------------------------------
// --zip: download folder as a zip archive
// ---------------------------------------------------------------------------

mod zip;
use zip::*;

mod run;
pub use run::run;
