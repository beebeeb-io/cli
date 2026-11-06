//! Which storage pool a file's bytes went to, as far as a user may be told
//! (spec §4.4): the pool's id and its country. Nothing else from the server's
//! answer is kept, so nothing else can be printed.

/// The pool of one file. Every field is optional: an older server, or a
/// download (whose file rows carry no pool id), answers with less.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PoolLabel {
    pub(crate) id: Option<uuid::Uuid>,
    pub(crate) country: Option<String>,
    pub(crate) country_code: Option<String>,
}
