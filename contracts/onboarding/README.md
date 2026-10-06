# Onboarding contract v1

Contract for `GET /api/v1/onboarding` (task 1752, epic 1725, spec T15). Source of truth:
`docs/specs/2026-10-04-backend-driven-onboarding.md` rev 3, section 5. **The endpoint is not
implemented yet (task 1739, T2).** This directory defines what it must emit.

| File | What |
|---|---|
| `schema.v1.json` | JSON Schema 2020-12 for schema major 1, as the SERVER emits it |
| `fixtures/*.json` | Golden documents, each validates against the schema |
| `invalid/*.json` | Hand-made breaking documents, each MUST be rejected by the schema (one per closed hole: a required unfinished step with `blocking: false`, unknown required step without fallback, `update_required` without an `update_app` fallback, unknown capability name, verified email with an `email_unverified` denial). The workspace guard does not assert these yet; run `bunx ajv-cli@5 validate ... -d invalid/<file>` and expect exit 1 |

## Fixtures

One per `account.state` value (12), plus platform and forward-compat variants (20 files):

| Fixture | Shows |
|---|---|
| `pre_account.{web,ios,desktop}` | signed-out signup steps, policy block (spec 5.4 A) |
| `account.allowance.{ios,desktop,web}` | allowance; iOS has no `offers`, no purchase CTA; desktop/web offer the no-card trial (5.4 B, C) |
| `account.trialing_no_card.desktop` | no-card trial running, over the allowance (5.4 D) |
| `account.trial_ended.ios` | trial over, usage above the allowance, deletion date (5.4 E) |
| `account.needs_plan.web.coupon` | task 1814: a plan-less web account that arrived through a coupon link: `offers.coupon` (free, 3 months of Pro) and an optional `redeem_coupon` step ahead of `choose_plan`. iOS never gets either (`offers` is omitted when `cta_allowed` is false) |
| `account.{needs_plan,trialing,trial_cancelling,active,past_due,read_only,frozen,lapsed,legacy_free}.*` | remaining states; values are illustrative where the spec gives none (see open questions) |
| `client.update_required.ios` | `client.status = update_required` with `fallback.kind = update_app` |
| `forward_compat.unknown_step.ios` | unknown required step id, unknown optional step id, unknown top-level and policy fields |

## Versioning and forward-compatibility rules (spec 5.8)

1. `schema` is an integer major. The client sends the highest it understands in `X-Beebeeb-Onboarding-Schema`; the server never answers above it. Breaking changes (removed or re-typed fields) need a new major (`schema.v2.json`, new fixtures) and a deprecation window of at least one release per client.
2. Within a major, changes are additive: new optional fields, new step ids, new enum values. (Task 1814 is one: `offers.coupon` and the optional `redeem_coupon` step, sent only when the client sends the optional `X-Beebeeb-Coupon` request header.)
3. Clients ignore unknown fields. **This schema is for validating server output and fixtures; clients must not use it as a strict parser.**
4. Unknown step id: if `required: true`, stop at that step and show the step `fallback` or the document `fallback` (the schema requires one of the two); if `required: false`, skip silently. The server never marks `verify_email_code`, `verify_email`, `create_account` as `required: false`, and never marks the pre-account `accept_terms` as `required: false` (the schema enforces it). **Task 1822 (decision 1812 Q2):** in the ACCOUNT document `accept_terms` is advisory for an existing account that has not accepted the Terms version in force: `required: false` (the schema enforces that too, so it can never make the document `blocking`). A client that renders it shows an Accept action and may skip it; a client that does not know the step skips it silently like any other optional step.
5. Unknown step `status` is `blocked`. Unknown `purchase.surface` is `none`. Unknown `account.state`: decide from `capabilities`, the state is only a label. `capabilities` is a closed set per major: absent means not allowed, and the schema rejects any name outside `download`, `upload`, `share`, `delete`.
6. Fetch failure or 404 on an old server: fall back to the legacy `/billing/subscription` `account_state` logic.
7. Money fails closed: `purchase.cta_allowed = false` means no `offers` and no `start_trial` step (the schema enforces both).
8. Crypto stays in `beebeeb-core`: the document carries parameters only, never keys, phrases or the email. `policy.password.breach_check.endpoint` is a same-origin path template (`/api/v1/...`), never an absolute URL.

Schema choices that follow from rule 3: `step.id` is an open string; enums elsewhere list the v1 values the server may emit.

## How clients vendor a copy

Copy this whole directory (`schema.v1.json`, `fixtures/`, this README; byte-for-byte, the guard diffs the whole tree) to:

| Client | Path |
|---|---|
| web | `repos/web/src/contracts/onboarding/` |
| mobile | `repos/mobile/src/contracts/onboarding/` |
| desktop | `repos/desktop/contracts/onboarding/` |
| cli | `repos/cli/contracts/onboarding/` |

```sh
cp -R repos/server/contracts/onboarding repos/web/src/contracts/onboarding   # example
make check-onboarding-contract        # from the workspace root
```

`scripts/check-onboarding-contract.sh` (workspace root) validates every fixture against the schema, requires a fixture per `account.state`, and diffs every existing client copy byte-for-byte against this directory (README included). It prints the number of client copies it found, including `0 client copies`. `--self-test` (run by `make guards-selftest`) proves it goes red on a broken fixture, an offer on iOS, a drifted copy and a missing fixture.

Validator: `bunx ajv-cli@5 validate --spec=draft2020 --strict-types=false -s schema.v1.json -d <fixture>` (fetched on demand; no format plugin needed, timestamps use a pattern).

Conformance of the real T2 output (task 1739) against this schema is added when T2 lands.

## Open contract questions

Found while writing the schema; nothing was silently resolved. Round 2 (2026-10-04) resolved the ones the spec rev 3 answers, citing the section; the rest stay open for Guus or T2 (task 1739). Rulings used: D13 trial 14 days / 10 GB cap, D15 read-only 14 days then delete, all other D-decisions at their recommendation (task 1736).

Resolved:

5. **`capabilities` names. Resolved.** Spec 5.3 and 5.8 rule 11: closed set per major, absent means not allowed. The schema now closes the object (`additionalProperties: false`): a misspelling or a new name fails validation, and a new capability needs a new schema major. The v1 name set (`download`, `upload`, `share`, `delete`) is the only thing still our choice: the spec names `share.limit.active_links` and `upload.limit_bytes` but only lists `delete` in prose (4b). `download`, `upload`, `share` stay required; `delete` stays optional. Task 1807 (ruling Q3): the server now states `delete` on every account document (it is the trash routes' gate: frozen, then billing read-only or suspended, nothing else), so for a v1 server it is never absent; it stays optional in the schema so an older server's document still validates, and a client treats absent as not allowed.
7. **`email_code.resend_after_seconds` is 60, `ttl_seconds` is 900. Resolved by ruling (Guus, 2026-10-05, task 1738): resend sends a fresh code.** Rev 3 had this equal to `ttl_seconds` (900 = 900, "resend is a server-side no-op while a live code exists", spec 5.9), and ruled a shorter resend out because it "would need the server to issue a fresh code on resend". The ruling changes exactly that: a resend now issues a fresh code, up to three live codes per address, at least `resend_after_seconds` (60) apart; a request inside that window, or with three codes live, is a server-side no-op with the same 202. The client shows a countdown from `resend_after_seconds` ("Send a new code in 0:42"), identical for known and unknown addresses (the server gives both the same schedule and byte-identical responses; a known address is mailed the "you already have an account" notice instead of a code). Any live code verifies and retires the rest; they share one wrong-guess budget (5). ~~`resend_after_seconds` equals `ttl_seconds`; 900 = 900 is deliberate; a shorter resend is NOT adopted~~ (struck, superseded by the ruling above; fixtures changed 900 to 60 in the same PR as the server behaviour).
8. **`update_required` forces `blocking: true` and a fallback. Resolved for `update_required` only.** Spec 5.8 rule 7: it blocks everything except Sign out and the store update. Round 2 tightens it: the fallback must have `kind: update_app` (a `use_web` or `contact_support` fallback is rejected). `blocking` for the other states is still open (see 8b).
11. **Unknown required step. Resolved (new in round 2).** Spec 5.8 rule 3 (stop at an unknown required step and show the step or document fallback): the schema now requires one of the two.

Still open:

1. **`trial_ended` definition contradicts itself.** 5.6 defines it as "used > allowance, deletion deadline pending". 4.5 has a row "`trial_ended`, usage at or under the allowance" (shown as plain `allowance`), and 4b.8 maps "`trial_ended` that was trimmed back" to legacy `needs_plan`. Schema: `trial_ended` accepts legacy `lapsed` or `needs_plan`, requires `trial` and `lifecycle`; no fixture for the at-or-under case. D15 (read-only 14 days, then delete) fixes the deadline length, not this definition.
2. **`offers.trial.unavailable_reason: not_offered_here`** (5.3 lists it) vs "`offers` is omitted entirely when `cta_allowed` is false" (4b.8, 5.3). If it is omitted there, the value is never sent. Schema: value allowed, `offers` forbidden when `cta_allowed = false`.
3. **Where `plans_managed_on_web` lives.** 5.3 puts it in top-level `copy`; examples B and E put it in `purchase.copy`. Schema allows both (`copy` is a string map). Fixtures follow the examples.
4. **Required top-level fields.** The 5.3 table lists `generated_at`, `ttl_seconds`, `client`, `copy`, `fallback` but the examples omit them inconsistently, and example A (pre-account) has no `purchase`. Schema requires only `schema`, `stage`, `steps`, `blocking`, plus `signup`+`policy` (pre-account) or `account`+`purchase` (account).
6. **Undefined vocabularies:** `signup.reason` values; `capabilities.*.reason` for read-only and frozen (fixtures use `billing_read_only`, `account_frozen`, invented); `lifecycle.reason` beyond `trial_ended`; `purchase.checkout.return_kind` beyond `app`; `price_visibility` `n/a` appears only in the matrix (cli row), allowed. Fixtures for `read_only`, `frozen`, `lapsed`, `past_due`, `trial_cancelling`, `trialing`, `legacy_free`, `needs_plan` use illustrative values; 5.6 itself marks `past_due` upload/share as "inferred, confirm in T2" and `frozen` download as "to confirm".
8b. **`blocking` for other states.** 4b.8 states it for `allowance`/`trialing_no_card`/`trial_ended` (false) and `needs_plan` (true). Other states are unspecified; the schema leaves them free. **Task 1740 refinement:** the `false` for `allowance`/`trialing_no_card`/`trial_ended` holds only while no required step is outstanding. **Task 1822 supersedes the part of this that said an unaccepted `accept_terms` is a required step:** an existing account that has not accepted the Terms version in force carries an ADVISORY `accept_terms` step (`required: false`, spec 5.5, D10 + decision 1812 Q2), so it does not make the document blocking. The rule itself stands for any other required step: 5.3 says a required unfinished step blocks, so `blocking: true` validates then. Rule: a `true` with no outstanding required step is still rejected for those three states.
9. **`allowance_bytes` / `over_allowance` outside the three allowance states.** Required for `allowance`, `trialing_no_card`, `trial_ended` only; whether other states may or must send them is not specified.
10. **CLI.** `signup.mode = web_only` is specified but the `reason` value and whether a CLI gets `pre_account` steps are not; no CLI fixture was written rather than invent them.

(Numbering is kept from round 1 so earlier references still resolve; 8b is the open half of old 8, 11 is new in round 2. Count: 4 resolved (5, 7, 8 for `update_required`, 11), 8 still open (1, 2, 3, 4, 6, 8b, 9, 10).)
