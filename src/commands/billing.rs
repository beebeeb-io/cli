//! `bb billing show` — read-only plan + storage + renewal info.
//!
//! Spec: docs/superpowers/specs/2026-05-23-cli-launch-readiness-design.md §4
//! "bb billing show output".
//!
//! ## `usage` (task 0486, plan Task 18)
//!
//! `GET /api/v1/billing/usage` (`ApiClient::get_billing_usage`, the same
//! billing-namespaced route `show` reads — task 0485 established this is the
//! server-authoritative one, NOT `get_usage()` which hits the different
//! `/api/v1/files/usage` route). Adds a per-region storage breakdown on top of
//! what `show`/`bb quota` already render.
//!
//! **Deviation (eng-0486) — region breakdown is client-side and approximate.**
//! The plan's open question #4 is still open: no
//! `GET /api/v1/billing/usage/by-region` route exists on the live server
//! (confirmed: `grep by.region beebeeb-api/src/routes/billing.rs` — no hit).
//! So, exactly as the plan's Task 18 describes, this aggregates locally over
//! `GET /files` with no `parent_id` (`ApiClient::list_files(None)`) — ONE
//! round-trip, root-level files only, no subfolder recursion — and labels the
//! breakdown "approximate" in both the rich and JSON output. Each file's
//! `storage_location.{region,country}` (present on `GET /files` rows —
//! `beebeeb-api/src/routes/files.rs`, "Storage location on file responses" in
//! the server's CLAUDE.md; task 2050: the unit is the country, the deprecated
//! `city` is never read) is the aggregation key.
//!
//! **Reuse, not reinvention.** The bar is `ui::quota_bar` (already used by
//! `bb quota`) rather than a new bespoke progress-bar renderer — the plan's
//! pseudocode sketches its own `render_progress_bar`, but this repo already
//! has one with the right green/amber/red thresholds baked in.
//!
//! ## `invoices` (task 0487, plan Task 19)
//!
//! `GET /api/v1/billing/invoices` (`ApiClient::get_billing_invoices`) —
//! `beebeeb-api/src/routes/billing.rs::invoices`.
//!
//! **Deviations (eng-0487) from the plan's Task 19 pseudocode, read against
//! the live handler** (confirmed against `repos/web/src/lib/api.ts`, which
//! reads the same route the same way):
//!
//! - Amount field is `amount_gross_cents` (VAT included), not `amount_paid`.
//! - `invoice_date`/`period_start`/`period_end` are ISO `YYYY-MM-DD` date
//!   strings (`chrono::NaiveDate::to_string()` server-side), NOT Unix
//!   timestamps — there is no epoch-seconds field to feed
//!   `DateTime::from_timestamp`.
//! - The PDF field is `pdf_url` (not `invoice_pdf`), and it is a RELATIVE,
//!   Bearer-auth-scoped path (`/api/v1/billing/invoices/{id}/pdf`) — never a
//!   public link a bare `curl` can fetch (confirmed: the handler takes an
//!   `AuthUser` extractor and 404s a non-owned id). `--open <id>` downloads it
//!   through the authenticated `ApiClient` (`download_invoice_pdf`) and opens
//!   the SAVED FILE locally — a raw browser URL would just 401.
//! - The response envelope is `{ invoices: [...] }` with no provider-
//!   configuration flag. Emptiness of the `invoices` array alone decides the
//!   "no invoices yet" message — never a configuration field (a gate on one
//!   would print "not configured" on every account, including ones with real
//!   invoices). **Not implemented.**
//! - `--open <id>` (from the task file's "Why", not in the plan's code
//!   sketch) accepts a full UUID or a unique prefix, reusing the
//!   `resolve_passkey_id`/`resolve_session_id` convention already established
//!   in this repo (`short_id`/`resolve_invoice_id` below).
//! - **Status vocabulary is `'draft' | 'issued' | 'void'`, not the
//!   `'paid'/'open'` pair** (`invoices` table CHECK constraint, `beebeeb-api/src/db.rs`;
//!   confirmed live by manual verification — every seeded row came back
//!   `status: "issued"` and, before this fix, rendered red as an unmatched
//!   status). See `invoice_status_color` for the live mapping.
//!
//! Owns: the `bb billing` command surface, re-exported from the children.
//! Must not: hold logic — `show` renders plan/storage, `usage` the region
//! breakdown, `invoices` the invoice list and PDF, `addons` the add-on flows.

use std::collections::BTreeMap;

use beebeeb_types::quota::{Plan, effective_quota, format_storage_si};
use chrono::DateTime;
use colored::Colorize;
use serde_json::Value;

use crate::api::ApiClient;
use crate::ui;

// ─── Helpers ─────────────────────────────────────────────────────────────────

mod show;
use show::*;
pub use show::show;

mod usage;
pub use usage::usage;

mod invoices;
pub use invoices::invoices;

mod addons;
use addons::*;
pub use addons::{addons, portal, purchase_addon};
