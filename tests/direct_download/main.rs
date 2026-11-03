//! `bb pull` over the direct S3 transport (task 2173, contract v1).
//!
//! Drives the real `bb` binary against two in-process mocks: an API (axum) that
//! speaks discovery, download grants, the pinned per-chunk route and today's
//! `/download`, and a STORAGE server written by hand on a raw TCP socket, so a
//! test can send exactly the bytes it wants: a short body, a wrong
//! `Content-Length`, a redirect, a tampered chunk. Every run has a scratch
//! `HOME` (never the developer's session) and `BB_NO_UPDATE=1`.
//!
//! The file is 21 chunks, so the transfer needs three windows of 8 grants.
//! Every scenario asserts the output is byte-identical to the plaintext, which
//! transport carried it (`bb pull --json` reports it), and what the mocks saw.
//! No scenario may print a signed link: `assert_clean` greps stdout and stderr
//! for the signature of every link the API issued.

mod support;
