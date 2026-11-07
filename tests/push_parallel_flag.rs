//! Task 6 of the `--parallel` plan (spec §5.1, §5.2, §8): the flag, its default and range, the additive `--json`
//! keys, and Ctrl-C through the real binary: a first Ctrl-C leaves a resumable record and exits 130, stops a
//! folder push as a whole, waits for a `complete` already in flight; a second exits at once; a completed session
//! met on resume is finished, never re-sent. Black box: the real `bb` against the in-process vault mock, scratch
//! `HOME`, loopback only; the signal goes to the pid the test recorded.

#[path = "support/push_vault.rs"]
mod vault;
use vault::*;

/// 4 x 4 MiB + 1000 bytes: five chunks at the 4 MiB floor, for every `--parallel`.
fn five_chunks() -> Vec<u8> {
    (0..4 * 4 * 1024 * 1024 + 1000usize)
        .map(|i| (i as u8).wrapping_mul(29).wrapping_add((i >> 13) as u8))
        .collect()
}

/// Send SIGINT to the child the test spawned (its own recorded pid, never a name).
fn ctrl_c(child: &std::process::Child) {
    let status = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("kill -INT");
    assert!(status.success(), "SIGINT delivered");
}

/// Wait until a session with at least two chunks holds chunk 0 (the push is mid-file, the next chunks stalled),
/// then a beat more so the client has seen chunk 0's answer.
async fn wait_for_chunk_0(vault: &Shared, child: &mut std::process::Child) {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let started = vault
            .lock()
            .unwrap()
            .sessions
            .values()
            .any(|s| s.chunk_count >= 2 && s.frames.contains_key(&0));
        if started {
            break;
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!("bb exited before it could be interrupted: {status:?}");
        }
        assert!(Instant::now() < deadline, "the upload never got chunk 0 to the server");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(250)).await;
}

/// Wait until `complete` has committed (the mock holds its answer) for one session.
async fn wait_for_complete_held(vault: &Shared, child: &mut std::process::Child) {
    let deadline = Instant::now() + Duration::from_secs(120);
    while vault.lock().unwrap().completed_sessions.is_empty() {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("bb exited before complete: {status:?}");
        }
        assert!(Instant::now() < deadline, "complete never came");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
}

fn output_of(child: std::process::Child) -> Output {
    child.wait_with_output().expect("bb output")
}

fn whitespace_collapsed(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[tokio::test(flavor = "multi_thread")]
async fn parallel_default_is_4() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);

    let help = bb(&home, &["--api", &api, "push", "--help"]).await;
    assert_eq!(help.status.code(), Some(0));
    let text = whitespace_collapsed(&String::from_utf8_lossy(&help.stdout));
    assert!(
        text.contains(
            "--parallel <PARALLEL> How many chunks to send at once, 1 to 8 [default: 4]. A file whose chunks do \
             not fit the 640 MiB upload buffer gets fewer; the output says so."
        ),
        "the help text, verbatim: {text}"
    );

    // The parsed default: a run without the flag reports 4.
    write_file(&home, "one.bin", b"one chunk");
    let out = bb(&home, &["--api", &api, "--json", "push", "one.bin"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let json: Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(json["transfer"]["parallel_asked"], 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn parallel_0_and_9_are_usage_errors() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    write_file(&home, "one.bin", b"one chunk");
    for bad in ["0", "9"] {
        let out = bb(&home, &["--api", &api, "push", "--parallel", bad, "one.bin"]).await;
        assert_eq!(out.status.code(), Some(2), "--parallel {bad}: a usage error, exit 2");
        assert!(
            stderr(&out).contains("1..=8"),
            "--parallel {bad}: the range is named: {}",
            stderr(&out)
        );
    }
    assert_eq!(vault.lock().unwrap().inits, 0, "nothing was sent");
}

/// Review Focus 6: the `transfer` object keeps 2333's keys and gains exactly `parallel_asked` and `pauses`;
/// `max_chunks_in_flight` carries `used`.
#[tokio::test(flavor = "multi_thread")]
async fn json_keys_are_additive() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    write_file(&home, "five.bin", &five_chunks());

    let out = bb(&home, &["--api", &api, "--json", "push", "--parallel", "1", "five.bin"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let json: Value = serde_json::from_slice(&out.stdout).expect("json");
    let transfer = json["transfer"].as_object().expect("transfer object");
    let mut keys: Vec<&str> = transfer.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let mut want = vec![
        "direction",
        "files_done",
        "files_total",
        "bytes_done",
        "bytes_total",
        "bytes_transferred",
        "chunks_done",
        "chunks_total",
        "chunks_already_stored",
        "duration_ms",
        "avg_mbps",
        "peak_chunks_in_flight",
        "max_chunks_in_flight",
        "routes",
        "retries",
        "pools",
        "parallel_asked",
        "pauses",
    ];
    want.sort_unstable();
    assert_eq!(keys, want, "2333's 16 keys plus the two new ones, nothing else");
    assert_eq!(transfer["parallel_asked"], 1);
    assert_eq!(transfer["pauses"], 0);
    assert_eq!(transfer["max_chunks_in_flight"], 1, "used at --parallel 1");
    assert_eq!(transfer["chunks_total"], 5);

    // At --parallel 4 on a new cut of the file, `used` is 4 (4 MiB chunks fit the budget 17 times).
    write_file(
        &home,
        "five.bin",
        &five_chunks().iter().map(|b| b.wrapping_add(1)).collect::<Vec<_>>(),
    );
    let out = bb(
        &home,
        &[
            "--api",
            &api,
            "--json",
            "push",
            "--keep-both",
            "--parallel",
            "4",
            "five.bin",
        ],
    )
    .await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let json: Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(json["transfer"]["parallel_asked"], 4);
    assert_eq!(json["transfer"]["max_chunks_in_flight"], 4, "used at --parallel 4");
    assert!(json["transfer"]["peak_chunks_in_flight"].as_u64().unwrap() <= 4);
}

/// A first Ctrl-C mid-file: the chunks in flight are dropped, the record stays, the line says how far it got,
/// exit 130; the same command then finishes the file, re-sending only what the server did not hold.
#[tokio::test(flavor = "multi_thread")]
async fn ctrl_c_leaves_a_resumable_record() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    write_file(&home, "five.bin", &five_chunks());

    vault.lock().unwrap().stall = true;
    let mut child = spawn_bb_piped(&home, &["--api", &api, "push", "five.bin"]);
    wait_for_chunk_0(&vault, &mut child).await;
    ctrl_c(&child);
    let out = output_of(child);
    vault.lock().unwrap().stall = false;

    assert_eq!(out.status.code(), Some(130), "exit 130: {}", stderr(&out));
    assert!(
        stderr(&out)
            .lines()
            .any(|l| l
                == "Interrupted: 1 of 5 chunks of five.bin confirmed in this run. Run the same command to continue."),
        "the interrupt line, verbatim: {}",
        stderr(&out)
    );
    assert!(record_exists(&home), "the record names the session");
    {
        let v = vault.lock().unwrap();
        assert_eq!(v.inits, 1);
        assert!(
            v.sessions.values().all(|s| s.frames.len() < 5),
            "the file was left undone"
        );
    }

    let out = bb(&home, &["--api", &api, "push", "five.bin"]).await;
    assert_eq!(out.status.code(), Some(0), "the re-run finishes it: {}", stderr(&out));
    let v = vault.lock().unwrap();
    assert_eq!(v.inits, 1, "the session was continued, not re-inited");
    assert_eq!(v.skipped, 1, "only chunk 0 was already held; the rest were stored now");
    let files = files_named(&v, "five.bin");
    assert_eq!(files.len(), 1);
    assert!(files[0].2, "completed");
    drop(v);
    assert!(!record_exists(&home), "the record is cleared");
}

/// A folder push stops as a whole at the first Ctrl-C: the files after the interrupted one are never inited,
/// one interrupt line, exit 130; the same command then finishes the folder. One file at a time (`--parallel 1`),
/// so "the files after it" are the ones not yet started; `push_parallel_files.rs` covers several files at once.
#[tokio::test(flavor = "multi_thread")]
async fn ctrl_c_stops_a_folder_push() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    write_file(&home, "clip/a.bin", b"one chunk, first");
    write_file(&home, "clip/b.bin", &five_chunks());
    write_file(&home, "clip/c.bin", b"one chunk, last");

    vault.lock().unwrap().stall = true;
    let mut child = spawn_bb_piped(&home, &["--api", &api, "push", "--parallel", "1", "clip"]);
    wait_for_chunk_0(&vault, &mut child).await;
    ctrl_c(&child);
    let out = output_of(child);
    vault.lock().unwrap().stall = false;

    assert_eq!(out.status.code(), Some(130), "{}", stderr(&out));
    let lines = stderr(&out).lines().filter(|l| l.starts_with("Interrupted: ")).count();
    assert_eq!(lines, 1, "exactly one interrupt line: {}", stderr(&out));
    assert_eq!(vault.lock().unwrap().inits, 2, "a and b inited; c never");
    assert!(record_exists(&home), "b's record is on disk");

    let out = bb(&home, &["--api", &api, "push", "--parallel", "1", "clip"]).await;
    assert_eq!(
        out.status.code(),
        Some(0),
        "the re-run finishes the folder: {}",
        stderr(&out)
    );
    let v = vault.lock().unwrap();
    assert_eq!(v.inits, 3, "b continued (no init), c inited");
    for name in ["a.bin", "b.bin", "c.bin"] {
        let files = files_named(&v, name);
        assert_eq!(files.len(), 1, "{name}");
        assert!(files[0].2, "{name} completed");
    }
}

/// A first Ctrl-C while `complete` is in flight: that request is waited for, the file is reported uploaded, the
/// record is cleared, exit 0 (nothing was left undone).
#[tokio::test(flavor = "multi_thread")]
async fn first_ctrl_c_during_complete_waits_for_it() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    write_file(&home, "two.bin", &two_chunks(1));

    vault.lock().unwrap().hold_complete = true;
    let mut child = spawn_bb_piped(&home, &["--api", &api, "push", "two.bin"]);
    wait_for_complete_held(&vault, &mut child).await;
    ctrl_c(&child);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        child.try_wait().unwrap().is_none(),
        "the push waits for complete's answer"
    );
    vault.lock().unwrap().hold_complete = false;
    let out = output_of(child);

    assert_eq!(out.status.code(), Some(0), "nothing left undone: {}", stderr(&out));
    assert!(!stderr(&out).contains("Interrupted:"), "{}", stderr(&out));
    assert!(!record_exists(&home), "the record is cleared");
    let v = vault.lock().unwrap();
    let files = files_named(&v, "two.bin");
    assert_eq!(files.len(), 1);
    assert!(files[0].2, "completed");
}

/// A second Ctrl-C in that window exits at once (130) and can leave the record: the re-run then meets a session
/// the server reports completed, and finishes it without sending a chunk (spec §5.2).
#[tokio::test(flavor = "multi_thread")]
async fn ctrl_c_during_complete_leaves_no_stuck_record() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    write_file(&home, "two.bin", &two_chunks(2));

    vault.lock().unwrap().hold_complete = true;
    let mut child = spawn_bb_piped(&home, &["--api", &api, "push", "two.bin"]);
    wait_for_complete_held(&vault, &mut child).await;
    ctrl_c(&child);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let second_at = Instant::now();
    ctrl_c(&child);
    let out = output_of(child);
    assert!(
        second_at.elapsed() < Duration::from_secs(5),
        "the second Ctrl-C exits at once"
    );
    vault.lock().unwrap().hold_complete = false;
    assert_eq!(out.status.code(), Some(130), "{}", stderr(&out));
    assert!(
        record_exists(&home),
        "the record is still on disk: complete never answered"
    );

    let out = bb(&home, &["--api", &api, "push", "two.bin"]).await;
    assert_eq!(
        out.status.code(),
        Some(0),
        "the completed session is finished: {}",
        stderr(&out)
    );
    let v = vault.lock().unwrap();
    assert_eq!(v.inits, 1, "never re-inited");
    assert!(v.sessions.is_empty(), "no chunk was stored into any session");
    let files = files_named(&v, "two.bin");
    assert_eq!(files.len(), 1, "one file in the folder");
    assert!(files[0].2, "completed");
    drop(v);
    assert!(!record_exists(&home), "the record is cleared");
}

/// `--quiet` prints none of the new lines: no interrupt line, no lowered-N line.
#[tokio::test(flavor = "multi_thread")]
async fn quiet_prints_no_new_lines() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    write_file(&home, "five.bin", &five_chunks());

    vault.lock().unwrap().stall = true;
    let mut child = spawn_bb_piped(&home, &["--api", &api, "--quiet", "push", "five.bin"]);
    wait_for_chunk_0(&vault, &mut child).await;
    ctrl_c(&child);
    let out = output_of(child);
    vault.lock().unwrap().stall = false;

    assert_eq!(out.status.code(), Some(130), "{}", stderr(&out));
    assert!(!stderr(&out).contains("Interrupted:"), "quiet: {}", stderr(&out));
    assert!(!stderr(&out).contains("chunks at once"), "quiet: {}", stderr(&out));
    assert!(out.stdout.is_empty(), "quiet prints the id only on success");
}
