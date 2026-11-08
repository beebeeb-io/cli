//! Task 1733 [P0]: `bb pull` must never write outside the directory the user
//! chose, whatever file or folder names the server hands back.
//!
//! The attack: an anonymous uploader with a victim's file-request link seals
//! the upload under a content key *they* chose, so the file name the victim's
//! CLI decrypts is entirely attacker-controlled (`../../.ssh/authorized_keys`,
//! an absolute path, ...). Before the fix `pull_folder_inner` did
//! `out_dir.join(&decrypted_name)` + `create_dir_all` + `fs::write` with no
//! validation and no containment check.
//!
//! These tests drive the REAL `bb` binary against an in-process mock API that
//! serves a folder tree whose names are malicious. Every run gets a scratch
//! `HOME` (the developer's real config holds a live session) and
//! `BB_NO_UPDATE=1`, so nothing leaves the loopback. They assert on the file
//! system: a snapshot of everything in the scratch tree *outside* the output
//! directory must be unchanged after the run.

mod support;
