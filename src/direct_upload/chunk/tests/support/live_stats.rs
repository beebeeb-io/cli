//! Task 2333 (spec §9.3 P4, §9.4 N2b): what the direct route tells the progress sink, and what a
//! verbose sink may show of it.

use std::sync::atomic::AtomicU64;

use super::*;
use crate::transfer_stats::Direction;
use crate::transfer_stats::absent::{PROVIDER_NAMES, assert_absent};
use crate::transfer_view::LiveProgress;
use crate::transfer_view::sink::Level;
use crate::upload::Recording;

/// P4: a resumed direct session whose chunk 0 the server already holds: chunk 0 is counted as
/// already stored (not sent), the rest as transferred, every chunk on the direct route.
#[tokio::test]
async fn direct_resume_reports_the_held_chunk() {
    let scratch = Scratch::new("2333-held");
    let (path, size, mtime) = scratch.file(FILE_BYTES);
    let chunks = chunk_count(size);
    let state = shared(|_| {});
    let (file_id, _) = resumed_session(&state, &path, size, mtime, true, &[0]);
    let rec = Recording::default();

    let outcome = run_with(&state, spec(&path, file_id), &rec).await.unwrap();

    let done = rec.of("done");
    assert_eq!(done.len() as u32, chunks, "{done:?}");
    assert!(
        done[0].starts_with("done 0 ") && done[0].ends_with(" already_stored"),
        "{done:?}"
    );
    assert!(done[1..].iter().all(|e| e.ends_with(" transferred")), "{done:?}");
    let starts = rec.of("start");
    assert_eq!(starts.len() as u32, chunks);
    assert!(starts.iter().all(|e| e.ends_with(" direct")), "{starts:?}");
    assert_eq!(rec.in_flight(), 0);
    let held: u64 = done[0].split(' ').nth(2).unwrap().parse().unwrap();
    assert_eq!(
        outcome.transferred_bytes,
        outcome.ciphertext_bytes - held,
        "the held chunk did not cross the wire"
    );
}

/// The direct policy's retry (a storage PUT refused once, granted again) is reported on its chunk.
#[tokio::test]
async fn direct_retry_is_reported() {
    let scratch = Scratch::new("2333-direct-retry");
    let (path, _, _) = scratch.file(FILE_BYTES);
    let state = shared(|m| m.storage_script = VecDeque::from([403]));
    let rec = Recording::default();

    run_with(&state, spec(&path, Uuid::new_v4()), &rec).await.unwrap();

    assert_eq!(rec.of("retry"), ["retry 0"]);
    assert_eq!(rec.in_flight(), 0);
}

/// Review P3: a storage PUT that landed but whose answer was lost (here a 503), then a grant that says `skipped`:
/// the chunk crossed the wire, so it is counted as transferred, not as already on the server.
#[tokio::test]
async fn direct_put_whose_answer_was_lost_counts_as_transferred() {
    let scratch = Scratch::new("2333-lost-answer");
    let (path, _, _) = scratch.file(FILE_BYTES);
    let state = shared(|m| {
        m.storage_script = VecDeque::from([503]);
        m.grant_script = VecDeque::from([
            Grant::Link,
            Grant::Reply(200, json!({"transport": CAP, "index": 0, "skipped": true}), None),
        ]);
    });
    let rec = Recording::default();

    let outcome = run_with(&state, spec(&path, Uuid::new_v4()), &rec).await.unwrap();

    assert_eq!(rec.of("retry"), ["retry 0"], "premise: the lost answer was retried");
    let done = rec.of("done");
    assert!(
        done[0].starts_with("done 0 ") && done[0].ends_with(" transferred"),
        "{done:?}"
    );
    assert_eq!(
        outcome.transferred_bytes, outcome.ciphertext_bytes,
        "every chunk crossed the wire"
    );
}

/// N2b: a verbose sink fed by the real direct upload shows no link, no signature, no storage host,
/// no bucket and no provider name: not in a live line, a chunk line, a notice, or the summary JSON.
#[tokio::test]
async fn verbose_direct_upload_renders_no_link() {
    let scratch = Scratch::new("2333-no-link");
    let (path, _, _) = scratch.file(FILE_BYTES);
    // One refused storage PUT: a retry and a second link are on the record too.
    let state = shared(|m| m.storage_script = VecDeque::from([403]));
    let (live, record) = LiveProgress::for_test(Direction::Upload, Level::Verbose, 200, Arc::new(AtomicU64::new(0)));

    let outcome = run_with(&state, spec(&path, Uuid::new_v4()), &live).await.unwrap();
    assert_eq!(
        outcome.transport, "direct",
        "premise: the direct route carried the file"
    );

    let mut shown = live.render_now();
    shown.extend(record.lock().unwrap().iter().cloned());
    shown.push(live.summary().to_json().to_string());
    let text = shown.join("\n");
    assert!(
        text.contains("    chunk 1/"),
        "premise: the chunk lines were recorded: {text}"
    );
    let storage = snapshot(&state, |m| m.storage.clone());
    let link = format!("http://{storage}/bb-test/");
    assert_absent(&text, &[&link, "SIGSECRET", "X-Amz-Signature", &storage, "bb-test"]);
    assert_absent(&text, &PROVIDER_NAMES);
}
