//! PR 2 of the `--parallel` plan (spec §7, Task 8): the files of a folder upload N at once. Stage 1 prepares every
//! file in order without a lock (every question before the first byte); stage 2 uploads up to N files at once, at
//! most N chunks in flight across them, each file taking its upload lock on admission and re-checking under it
//! what stage 1 saw. Black box: the real `bb` against the in-process vault mock, scratch `HOME`, loopback only;
//! signals go to the pid the test recorded.

#[path = "support/push_vault.rs"]
mod vault;
use vault::*;

/// The session id of every chunk PUT the mock received, in arrival order (a held PUT counts from its arrival).
fn chunk_puts(v: &Vault) -> Vec<String> {
    v.requests
        .iter()
        .filter(|(method, path, _)| method == "PUT" && path.contains("/chunks/"))
        .filter_map(|(_, path, _)| path.split('/').nth(4).map(str::to_string))
        .collect()
}

fn count_requests(v: &Vault, method: &str, path: &str) -> usize {
    v.requests.iter().filter(|(m, p, _)| m == method && p == path).count()
}

/// Wait (up to 60 s) until `ready` holds for the vault, failing if `bb` exits first.
async fn wait_until(vault: &Shared, child: &mut std::process::Child, what: &str, ready: impl Fn(&Vault) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !ready(&vault.lock().unwrap()) {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("bb exited before {what}: {status:?}");
        }
        assert!(Instant::now() < deadline, "never: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Send SIGINT to the child the test spawned (its own recorded pid, never a name).
fn ctrl_c(child: &std::process::Child) {
    let status = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("kill -INT");
    assert!(status.success(), "SIGINT delivered");
}

fn output_of(child: std::process::Child) -> Output {
    let out = child.wait_with_output().expect("bb output");
    eprintln!(
        "[bb -> {:?}]\n--- stdout\n{}--- stderr\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// Every file named in `names` is in the vault exactly once, completed.
fn each_completed_once(v: &Vault, names: &[&str]) {
    for name in names {
        let files = files_named(v, name);
        assert_eq!(files.len(), 1, "{name}: {files:?}");
        assert!(files[0].2, "{name} completed");
    }
}

/// N = 4: chunks of several files are in flight together, and never more than 4 across them (20 files of two
/// chunks each, every chunk PUT held by the mock until the test lets go).
#[tokio::test(flavor = "multi_thread")]
async fn small_files_upload_n_at_once() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    let names: Vec<String> = (0..20).map(|i| format!("f{i:02}.bin")).collect();
    for (i, name) in names.iter().enumerate() {
        write_file(&home, &format!("many/{name}"), &two_chunks(i as u8));
    }

    vault.lock().unwrap().hold_chunks = true;
    let mut child = spawn_bb_piped(&home, &["--api", &api, "--json", "push", "--parallel", "4", "many"]);
    wait_until(&vault, &mut child, "4 chunk PUTs held", |v| chunk_puts(v).len() >= 4).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let held = chunk_puts(&vault.lock().unwrap());
    vault.lock().unwrap().hold_chunks = false;
    assert_eq!(held.len(), 4, "at most 4 chunks in flight across the files: {held:?}");
    let sessions: std::collections::HashSet<&String> = held.iter().collect();
    assert!(sessions.len() >= 2, "the 4 come from more than one file: {held:?}");

    let out = output_of(child);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let json: Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(json["total_files"], 20);
    assert_eq!(json["transfer"]["peak_chunks_in_flight"], 4, "{}", json["transfer"]);
    assert_eq!(json["transfer"]["max_chunks_in_flight"], 4, "{}", json["transfer"]);
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    each_completed_once(&vault.lock().unwrap(), &names);
}

/// N = 1: one chunk in flight, and the files go one after another: each file's init, chunks and complete come
/// before the next file's init (today's per-file request order).
#[tokio::test(flavor = "multi_thread")]
async fn parallel_1_sends_one_file_at_a_time() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    for (i, name) in ["a.bin", "b.bin", "c.bin"].iter().enumerate() {
        write_file(&home, &format!("three/{name}"), &two_chunks(i as u8));
    }

    vault.lock().unwrap().hold_chunks = true;
    let mut child = spawn_bb_piped(&home, &["--api", &api, "push", "--parallel", "1", "three"]);
    wait_until(&vault, &mut child, "a chunk PUT held", |v| !chunk_puts(v).is_empty()).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let held = chunk_puts(&vault.lock().unwrap()).len();
    vault.lock().unwrap().hold_chunks = false;
    assert_eq!(held, 1, "one chunk in flight at --parallel 1");
    let out = output_of(child);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));

    // The upload requests, reduced to `init`, `chunk:<sid>`, `complete:<sid>`: never interleaved between files.
    let v = vault.lock().unwrap();
    let mut open: Option<String> = None;
    let mut files = 0;
    for (method, path, _) in &v.requests {
        if path == "/api/v1/uploads/init" {
            assert!(open.is_none(), "an init while a file was still open: {:?}", v.requests);
            open = Some(String::new());
        } else if let Some(rest) = path.strip_prefix("/api/v1/uploads/") {
            let sid = rest.split('/').next().unwrap_or("").to_string();
            let current = open.as_mut().expect("an upload request outside a file");
            if current.is_empty() {
                *current = sid.clone();
            }
            assert_eq!(
                *current, sid,
                "{method} {path}: another file's session while one was open"
            );
            if path.ends_with("/complete") {
                open = None;
                files += 1;
            }
        }
    }
    assert_eq!(files, 3);
    each_completed_once(&v, &["a.bin", "b.bin", "c.bin"]);
}

/// Spawn `bb <args>` with a pty as its controlling terminal (stdin), its stdout and stderr piped, and hand back the
/// child and the pty's master end, so the test types each answer when it chooses.
#[cfg(unix)]
fn spawn_at_terminal(home: &Path, args: &[&str]) -> (std::process::Child, std::fs::File) {
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;

    let (master, slave) = unsafe {
        let (mut m, mut s) = (0, 0);
        let rc = libc::openpty(
            &mut m,
            &mut s,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        assert_eq!(rc, 0, "openpty failed");
        (std::fs::File::from_raw_fd(m), OwnedFd::from_raw_fd(s))
    };
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_bb"));
    cmd.env("BB_REFUSE_NON_LOOPBACK_API", "1");
    cmd.args(args)
        .current_dir(home)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("BB_NO_UPDATE", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::from(slave))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn().expect("spawn bb");
    // Drain the terminal, or a session leader's exit blocks on closing it.
    let mut tty_out = master.try_clone().unwrap();
    std::thread::spawn(move || {
        let mut sink = [0u8; 1024];
        while matches!(std::io::Read::read(&mut tty_out, &mut sink), Ok(n) if n > 0) {}
    });
    (child, master)
}

/// Spec §7: every question is asked before the first byte goes out. Two files of a re-pushed folder meet their
/// earlier, now different, uploads; the first question is answered, the second is not yet: the push must be at the
/// second question with no upload begun (under one-file-at-a-time, the first file would already be up).
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn all_questions_before_the_first_byte() {
    use std::io::Write;
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    write_file(&home, "dup/a.txt", b"first a");
    write_file(&home, "dup/b.txt", b"first b");
    let out = bb(&home, &["--api", &api, "push", "dup"]).await;
    assert_eq!(out.status.code(), Some(0), "first push: {}", stderr(&out));
    write_file(&home, "dup/a.txt", b"second a, longer");
    write_file(&home, "dup/b.txt", b"second b, longer");
    {
        let mut v = vault.lock().unwrap();
        v.requests.clear();
        v.inits = 0;
    }

    let (mut child, mut tty) = spawn_at_terminal(&home, &["--api", &api, "push", "--parallel", "4", "dup"]);
    tty.write_all(b"k\n").unwrap();
    wait_until(&vault, &mut child, "both files' conflict lookups", |v| {
        count_requests(v, "POST", "/api/v1/files/check-conflict") >= 2
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    {
        let v = vault.lock().unwrap();
        assert_eq!(
            v.inits, 0,
            "no upload began before the second question: {:?}",
            v.requests
        );
        assert!(
            chunk_puts(&v).is_empty(),
            "no chunk went out before the second question"
        );
    }
    tty.write_all(b"k\n").unwrap();
    let out = output_of(child);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let prompts = stderr(&out).matches("already exists.").count();
    assert_eq!(prompts, 2, "two questions: {}", stderr(&out));
    each_completed_once(&vault.lock().unwrap(), &["a (1).txt", "b (1).txt"]);
}

/// One file that fails does not stop the others, at N = 4: its line, the rest uploaded, the folder's failure exit.
#[tokio::test(flavor = "multi_thread")]
async fn one_failed_file_does_not_stop_the_others() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    for i in 1..=5 {
        write_file(&home, &format!("five/f{i}.bin"), format!("file {i}").as_bytes());
    }
    vault.lock().unwrap().refuse_init_named = Some("f2.bin".to_string());

    let out = bb(&home, &["--api", &api, "push", "--parallel", "4", "five"]).await;
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("1 of 5 files failed to upload: f2.bin"),
        "{}",
        stderr(&out)
    );
    let v = vault.lock().unwrap();
    each_completed_once(&v, &["f1.bin", "f3.bin", "f4.bin", "f5.bin"]);
    assert!(files_named(&v, "f2.bin").is_empty());
}

/// `--json` lists the files in input order, whatever order they finish in: a five-chunk file first, three small
/// ones after it that finish before it.
#[tokio::test(flavor = "multi_thread")]
async fn json_results_keep_input_order() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    let big: Vec<u8> = (0..4 * 4 * 1024 * 1024 + 1000usize)
        .map(|i| (i as u8).wrapping_mul(29).wrapping_add((i >> 13) as u8))
        .collect();
    write_file(&home, "mixed/a.bin", &big);
    for name in ["b.bin", "c.bin", "d.bin"] {
        write_file(&home, &format!("mixed/{name}"), name.as_bytes());
    }

    let out = bb(&home, &["--api", &api, "--json", "push", "--parallel", "4", "mixed"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let json: Value = serde_json::from_slice(&out.stdout).expect("json");
    let order: Vec<&str> = json["files"]
        .as_array()
        .expect("files")
        .iter()
        .map(|f| f["name"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(order, ["a.bin", "b.bin", "c.bin", "d.bin"]);
    // The order is the input's, not the finish order: a small file finished before the big one.
    let v = vault.lock().unwrap();
    let finished: Vec<String> = v
        .requests
        .iter()
        .filter_map(|(_, p, _)| p.strip_suffix("/complete")?.strip_prefix("/api/v1/uploads/"))
        .filter_map(|sid| v.completed_sessions.get(sid))
        .filter_map(|id| v.items.get(id))
        .map(name_of)
        .collect();
    assert_eq!(finished.len(), 4, "{finished:?}");
    assert_ne!(
        finished[0], "a.bin",
        "a small file finished first, so the order is not the finish order"
    );
}

/// Spawn `bb <args>` with its soft open-file limit lowered to `limit`, its output piped.
#[cfg(unix)]
fn spawn_with_fd_limit(home: &Path, args: &[&str], limit: u64) -> std::process::Child {
    use std::os::unix::process::CommandExt;
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_bb"));
    cmd.env("BB_REFUSE_NON_LOOPBACK_API", "1");
    cmd.args(args)
        .current_dir(home)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("BB_NO_UPDATE", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        cmd.pre_exec(move || {
            let mut rl = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            rl.rlim_cur = limit as libc::rlim_t;
            if libc::setrlimit(libc::RLIMIT_NOFILE, &rl) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn().expect("spawn bb")
}

/// Spec §7: a file takes its upload lock on admission, so at most N are open at once: 300 files at N = 8 under a
/// soft limit of 256 open files all upload (stage 1 holding every lock would run out of descriptors).
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn locks_are_taken_at_admission() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    for i in 0..300 {
        write_file(
            &home,
            &format!("lots/f{i:03}.bin"),
            format!("small file {i}").as_bytes(),
        );
    }

    let child = spawn_with_fd_limit(
        &home,
        &["--api", &api, "--json", "push", "--parallel", "8", "lots"],
        256,
    );
    let out = output_of(child);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let json: Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(json["total_files"], 300, "{}", stderr(&out));
    assert_eq!(json["total_failed"], 0);
}

/// A file whose upload lock another `bb push` holds when stage 2 reaches it fails with today's message; the
/// other files upload. The other push is a real `bb push` of that file, holding its lock at its own conflict
/// question (a same-name file in the root), which nobody answers.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_push_between_the_stages_is_refused_under_the_lock() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    for name in ["a.bin", "b.bin", "c.bin", "d.bin"] {
        write_file(&home, &format!("four/{name}"), name.as_bytes());
    }
    seed_file_row(&mut vault.lock().unwrap(), None, "c.bin");

    let (mut other, _tty) = spawn_at_terminal(&home, &["--api", &api, "push", "four/c.bin"]);
    wait_until(&vault, &mut other, "the other push's conflict lookup", |v| {
        count_requests(v, "POST", "/api/v1/files/check-conflict") >= 1
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let out = bb(&home, &["--api", &api, "push", "--parallel", "4", "four"]).await;
    other.kill().expect("stop the other push (its recorded pid)");
    let _ = other.wait();

    assert_eq!(
        out.status.code(),
        Some(75),
        "today's exit for a held lock: {}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("another bb push of c.bin is still running"),
        "today's message: {}",
        stderr(&out)
    );
    let v = vault.lock().unwrap();
    let folder = the_folder(&v, None, "four");
    assert_eq!(
        file_names_in(&v, &folder),
        ["a.bin", "b.bin", "d.bin"],
        "the other files uploaded into the folder"
    );
}

/// A same-name file that appears between the stages fails that file with its own line; the others upload.
/// `--parallel 1`, so the row (added when the first file inits) is there before the third file's re-check.
#[tokio::test(flavor = "multi_thread")]
async fn changed_while_preparing_fails_that_file() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    for name in ["a.bin", "b.bin", "c.bin"] {
        write_file(&home, &format!("prep/{name}"), name.as_bytes());
    }
    vault.lock().unwrap().add_on_first_init = Some("c.bin".to_string());

    let out = bb(&home, &["--api", &api, "push", "--parallel", "1", "prep"]).await;
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("c.bin: changed while this push was preparing; run the command again"),
        "{}",
        stderr(&out)
    );
    let v = vault.lock().unwrap();
    each_completed_once(&v, &["a.bin", "b.bin"]);
    assert_eq!(files_named(&v, "c.bin").len(), 1, "only the other device's c.bin");
}

/// Ctrl-C with two files in flight (N = 2): both stop, the files not started are never inited, one interrupt line
/// naming the folder, exit 130; the same command then finishes all four.
#[tokio::test(flavor = "multi_thread")]
async fn ctrl_c_stops_every_file() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    let names = ["a.bin", "b.bin", "c.bin", "d.bin"];
    for (i, name) in names.iter().enumerate() {
        write_file(&home, &format!("clip/{name}"), &two_chunks(i as u8));
    }

    vault.lock().unwrap().stall = true;
    let mut child = spawn_bb_piped(&home, &["--api", &api, "push", "--parallel", "2", "clip"]);
    wait_until(&vault, &mut child, "two files' chunk 0", |v| {
        v.sessions.values().filter(|s| s.frames.contains_key(&0)).count() >= 2
    })
    .await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    ctrl_c(&child);
    let out = output_of(child);
    vault.lock().unwrap().stall = false;

    assert_eq!(out.status.code(), Some(130), "{}", stderr(&out));
    let err = stderr(&out);
    let lines: Vec<&str> = err
        .lines()
        .filter(|l| l.starts_with("Interrupted: "))
        .map(str::trim)
        .collect();
    assert_eq!(
        lines,
        ["Interrupted: 2 of 8 chunks of clip confirmed in this run. Run the same command to continue."],
        "{}",
        stderr(&out)
    );
    assert_eq!(vault.lock().unwrap().inits, 2, "c and d never inited");
    assert!(record_exists(&home), "the records are on disk");

    let out = bb(&home, &["--api", &api, "push", "--parallel", "2", "clip"]).await;
    assert_eq!(out.status.code(), Some(0), "the re-run finishes: {}", stderr(&out));
    let v = vault.lock().unwrap();
    assert_eq!(v.inits, 4, "a and b continued (no init), c and d inited");
    each_completed_once(&v, &names);
}

/// Review P2 (#141): a Ctrl-C while stage 2's re-checks wait out a rate-limit pause stops the push as an interrupt
/// (one line, exit 130), never as failed files, and no internal sentinel reaches the person.
#[tokio::test(flavor = "multi_thread")]
async fn ctrl_c_during_the_recheck_pause_exits_130() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    for name in ["a.bin", "b.bin"] {
        write_file(&home, &format!("pair/{name}"), name.as_bytes());
    }
    // Stage 1 looks both up (calls 1 and 2); both re-checks of stage 2 meet a 429 and a 60 s pause.
    vault.lock().unwrap().conflict_429 = Some((3, usize::MAX, 60));

    let mut child = spawn_bb_piped(&home, &["--api", &api, "push", "--parallel", "2", "pair"]);
    wait_until(&vault, &mut child, "both re-checks rate limited", |v| {
        count_requests(v, "POST", "/api/v1/files/check-conflict") >= 4
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    ctrl_c(&child);
    let out = output_of(child);

    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(130), "{err}");
    assert_eq!(
        err.lines().filter(|l| l.starts_with("Interrupted: ")).count(),
        1,
        "{err}"
    );
    assert!(
        !err.contains("__interrupted__") && !err.contains("__rate_limited__"),
        "{err}"
    );
    assert_eq!(vault.lock().unwrap().inits, 0, "nothing was inited");
}

/// Review P2 (#141): a 429 on stage 2's re-check waits out the shared pause and asks again; the file uploads.
#[tokio::test(flavor = "multi_thread")]
async fn a_429_on_the_recheck_is_retried() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    write_file(&home, "one/a.bin", b"a");
    // Call 1 is stage 1's lookup; call 2, the re-check, answers 429 once.
    vault.lock().unwrap().conflict_429 = Some((2, 1, 1));

    let out = bb(&home, &["--api", &api, "push", "--parallel", "2", "one"]).await;
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(0), "{err}");
    assert_eq!(err.matches("pausing").count(), 1, "one pause notice: {err}");
    assert!(!err.contains("__rate_limited__"), "{err}");
    let v = vault.lock().unwrap();
    assert_eq!(
        count_requests(&v, "POST", "/api/v1/files/check-conflict"),
        3,
        "the re-check was asked again"
    );
    each_completed_once(&v, &["a.bin"]);
}

/// Review P2 (#141): a file that grows after stage 1 planned it, while two other files of the folder are in flight
/// (N = 2), is uploaded at its new size, and the run's totals follow it.
#[tokio::test(flavor = "multi_thread")]
async fn a_file_that_grew_counts_its_new_size_at_parallel_2() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    write_file(&home, "grow/a.bin", b"aaaa");
    write_file(&home, "grow/b.bin", b"bbbbbb");
    let c = write_file(&home, "grow/c.bin", b"c");
    vault.lock().unwrap().hold_init_named = ["a.bin", "b.bin"].iter().map(|s| s.to_string()).collect();

    let mut child = spawn_bb_piped(&home, &["--api", &api, "--json", "push", "--parallel", "2", "grow"]);
    wait_until(&vault, &mut child, "a and b's inits held", |v| {
        count_requests(v, "POST", "/api/v1/uploads/init") >= 2
    })
    .await;
    // c was planned at 1 byte; it grows past one 4 MiB chunk before its own upload begins.
    let grown = 5 * 1024 * 1024 + 7;
    std::fs::write(&c, vec![b'c'; grown]).unwrap();
    vault.lock().unwrap().hold_init_named.clear();
    let out = output_of(child);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));

    let json: Value = serde_json::from_slice(&out.stdout).expect("json");
    let t = &json["transfer"];
    // The real ciphertext: each chunk adds 28 bytes; c is two chunks at the 4 MiB floor.
    let real = (4 + 28) + (6 + 28) + (grown as u64 + 2 * 28);
    assert_eq!(t["bytes_total"], real, "{t}");
    assert_eq!(t["bytes_done"], t["bytes_total"], "{t}");
    assert_eq!(
        (t["chunks_done"].clone(), t["chunks_total"].clone()),
        (json!(4), json!(4)),
        "{t}"
    );
    assert_eq!(json["total_bytes"], real, "{json}");
}
