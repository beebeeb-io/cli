//! The direct upload of one chunk (task 2173): grant, `PUT` to the storage link, commit.
//!
//! The DECISIONS live in `beebeeb_core::direct_transfer` (shared with every client): what an answer
//! means, whether a link may be followed, how many links and how much waiting one chunk gets before
//! the file goes back to the API. This file makes the requests and reports what it saw. It prints no
//! link: the HTTP stack's errors never leave `ApiClient::put_to_storage`.
//!
//! A session's transport never changes after init (spec, "Transport exclusivity"), so the only
//! fallback is for the whole file: [`DirectUploadError::UseApi`] tells the upload driver to abandon
//! the session and init again without the capability token.
//!
//! Owns: the error the driver matches on, and nothing else; the transport itself is `chunk`.
//! Must not: decide the retry policy (that is `beebeeb_core::direct_transfer`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use beebeeb_core::direct_transfer as dt;
use bytes::Bytes;

use crate::api::{ApiClient, UploadCall};

mod error;
pub(crate) use error::DirectUploadError;

mod chunk;
pub(crate) use chunk::upload_chunk;
