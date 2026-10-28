#[path="support/shared_folder.rs"] mod sf;
use sf::*;
use serde_json::{Value,json};
const REFUSAL:&str="This folder was synced by a different account. Sign in as that account, or sync into a new folder.";
fn fixture()->Fixture {let mut f=Fixture::basic();f.rows.truncate(1);f}
fn manifest(h:&Harness,account:Option<&str>)->std::path::PathBuf {let dir=h.home.join("sync");std::fs::create_dir_all(&dir).unwrap();let mut v=json!({"remote_path":"/Shared","remote_folder_id":ROOT,"files":{}});if let Some(id)=account {v["account_id"]=json!(id);}std::fs::write(dir.join(".bb-sync.json"),v.to_string()).unwrap();dir}
fn state(dir:&std::path::Path)->Value {serde_json::from_slice(&std::fs::read(dir.join(".bb-sync.json")).unwrap()).unwrap()}
#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn mismatch_refuses_before_io(){let h=Harness::new(fixture()).await;let dir=manifest(&h,Some(BAD));let before=std::fs::read(dir.join(".bb-sync.json")).unwrap();let r=h.run(&["sync",dir.to_str().unwrap(),"--once"]).await;assert!(!r.status.success());assert!(String::from_utf8_lossy(&r.stderr).contains(REFUSAL));assert_eq!(h.count("GET","/api/v1/auth/me"),1);assert_eq!(h.state.lock().unwrap().requests.len(),1);assert_eq!(std::fs::read(dir.join(".bb-sync.json")).unwrap(),before);h.close().await;}
#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn legacy_readable_root_adopts_once(){let h=Harness::new(fixture()).await;let dir=manifest(&h,None);for _ in 0..2 {let r=h.run(&["sync",dir.to_str().unwrap(),"--once"]).await;assert!(r.status.success(),"{}",String::from_utf8_lossy(&r.stderr));assert_eq!(state(&dir)["account_id"],ACCOUNT);}assert_eq!(h.count("GET",&format!("/api/v1/files/{ROOT}")),1);h.close().await;}
#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn legacy_unreadable_root_refuses(){let mut f=fixture();f.canonical.insert(ROOT.into(),Value::Null);let h=Harness::new(f).await;let dir=manifest(&h,None);let r=h.run(&["sync",dir.to_str().unwrap(),"--once"]).await;assert!(!r.status.success());assert!(String::from_utf8_lossy(&r.stderr).contains(REFUSAL));assert_eq!(h.count("GET","/api/v1/files?"),0);assert!(state(&dir)["account_id"].is_null());h.close().await;}
#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn dry_run_never_writes_account_id(){let h=Harness::new(fixture()).await;let dir=manifest(&h,None);let before=std::fs::read(dir.join(".bb-sync.json")).unwrap();let r=h.run(&["sync",dir.to_str().unwrap(),"--once","--dry-run"]).await;assert!(r.status.success(),"{}",String::from_utf8_lossy(&r.stderr));assert_eq!(std::fs::read(dir.join(".bb-sync.json")).unwrap(),before);let fresh=h.home.join("fresh");std::fs::create_dir(&fresh).unwrap();let r=h.run(&["sync",fresh.to_str().unwrap(),"/Shared","--once","--dry-run"]).await;assert!(r.status.success());assert!(!fresh.join(".bb-sync.json").exists());h.close().await;}
impl Harness {
    pub async fn run(&self, args: &[&str]) -> std::process::Output {
        let home = self.home.clone();
        let api = self.api.clone();
        let direct = self.state.lock().unwrap().direct;
        let args: Vec<_> = args.iter().map(|s| s.to_string()).collect();
        tokio::task::spawn_blocking(move || {
            std::process::Command::new(env!("CARGO_BIN_EXE_bb"))
                .args(args)
                .args(["--api", &api, "--json"])
                .env("HOME", &home)
                .env("XDG_CONFIG_HOME", home.join(".config"))
                .env("BB_REFUSE_NON_LOOPBACK_API", "1")
                .env("BB_NO_UPDATE", "1")
                .env("BB_TRANSPORT", if direct { "auto" } else { "api" })
                .env_remove("APP_URL")
                .output()
                .unwrap()
        })
        .await
        .unwrap()
    }
}
