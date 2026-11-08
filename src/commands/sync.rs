//! `bb sync`: the facade over the pass (run), its model, its report and the
//! reconcile, watch, session, scan and transfer helpers.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Instant, SystemTime};

use base64::Engine;
use chrono::{DateTime, Utc};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use walkdir::WalkDir;
use zeroize::Zeroizing;

use crate::api::ApiClient;
use crate::config::load_config;
use crate::owned_keys::{CUSTODY_UNAVAILABLE, OwnedKeyResolver, ResolvedCliKey};
use crate::ui;
use beebeeb_core::client_meta::{ClientMetaField, fit_for_seal, open_client_meta, seal_client_meta};
use beebeeb_core::kdf::MasterKey;
use beebeeb_core::owned_keys::KeyOrigin;

const STATE_FILE: &str = ".bb-sync.json";
// Chunk size is now computed dynamically via beebeeb_types::plan_chunks().
// See the adaptive chunk-size ladder in beebeeb-types/src/chunk.rs.

mod model;
use model::*;
pub use model::{SyncCommand, SyncOptions};

mod run;
pub use run::run;

mod report;
use report::*;

mod reconcile;
use reconcile::*;

mod watch;
use watch::*;

mod session;
use session::*;

mod scan;
use scan::*;

mod transfer;
use transfer::*;

#[cfg(test)]
#[path = "sync/tests/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "sync/tests/l13_trash_after_upload_tests.rs"]
mod l13_trash_after_upload_tests;

#[cfg(test)]
#[path = "sync/tests/conflict_policy_tests.rs"]
mod conflict_policy_tests;

#[cfg(test)]
#[path = "sync/tests/a9b_request_edit_tests.rs"]
mod a9b_request_edit_tests;

#[cfg(test)]
#[path = "sync/tests/live_stats_tests.rs"]
mod live_stats_tests;

#[cfg(test)]
#[path = "sync/tests/short_name_tests.rs"]
mod short_name_tests;
