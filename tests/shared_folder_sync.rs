#[path="support/shared_folder.rs"]
mod sf;
use sf::*;
use serde_json::{json,Value};
use std::path::Path;
use beebeeb_core::search_index::{SearchIndex,ShardKeySource,EncryptedShard};
use base64::{Engine as _,engine::general_purpose::STANDARD as B64};
fn state(dir:&Path)->Value {serde_json::from_slice(&std::fs::read(dir.join(".bb-sync.json")).unwrap()).unwrap()}
fn dir(h:&Harness)->std::path::PathBuf {let d=h.home.join("sync");std::fs::create_dir(&d).unwrap();d}
async fn sync(h:&Harness,d:&Path)->std::process::Output {h.run(&["sync",d.to_str().unwrap(),"/Shared","--once","--rehash"]).await}
fn ok(r:std::process::Output){assert!(r.status.success(),"{} / {}",String::from_utf8_lossy(&r.stdout),String::from_utf8_lossy(&r.stderr));}
fn bytes(h:&Harness,d:&Path,name:&str){use sha2::{Digest,Sha256};assert_eq!(Sha256::digest(std::fs::read(d.join(name)).unwrap()),Sha256::digest(&h.state.lock().unwrap().plaintext));}
async fn repeats(h:&Harness,d:&Path){let before=h.count("POST","/api/v1/uploads/init");for _ in 0..3{ok(sync(h,d).await);}let repeat_uploads=h.count("POST","/api/v1/uploads/init")-before;assert_eq!(repeat_uploads,0);}
fn index(h:&Harness,shared:bool)->SearchIndex{let f=h.state.lock().unwrap();let shards=(if shared {&f.shared_index} else {&f.owner_index}).iter().map(|(&(bucket,page),(_,blob))|EncryptedShard{bucket,page,blob:blob.clone()}).collect::<Vec<_>>();let fk=folder_key();let mk=master();SearchIndex::from_encrypted_shards_with(if shared {ShardKeySource::Folder(&fk)}else{ShardKeySource::Master(&mk)},&shards,64).unwrap()}
#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn first_sync_adopts_and_downloads(){let h=Harness::new(Fixture::transfer(true)).await;let d=dir(&h);ok(sync(&h,&d).await);bytes(&h,&d,"guest.txt");assert_eq!(h.count("POST",&format!("/api/v1/files/{FILE}/owner-key")),1);assert_eq!(state(&d)["account_id"],ACCOUNT);assert!(index(&h,false).query("guest").contains(FILE));repeats(&h,&d).await;h.close().await;}
#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn restart_revoke_and_move_out(){let h=Harness::new(Fixture::transfer(false)).await;let d=dir(&h);ok(sync(&h,&d).await);bytes(&h,&d,"guest.txt");{let mut f=h.state.lock().unwrap();f.custody_revoked=true;f.inventory.clear();f.rows.push(row(SUB,Some(ROOT),"Moved",true,false));f.rows[1]["parent_id"]=json!(SUB);f.rows[1]["acted_via_share_id"]=Value::Null;}ok(sync(&h,&d).await);bytes(&h,&d,"Moved/guest.txt");assert!(!d.join("guest.txt").exists());repeats(&h,&d).await;h.close().await;}
#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn rename_move_overwrite_converge(){let h=Harness::new(Fixture::transfer(true)).await;let d=dir(&h);ok(sync(&h,&d).await);bytes(&h,&d,"guest.txt");{let mut f=h.state.lock().unwrap();f.rows.push(row(SUB,Some(ROOT),"Nested",true,false));f.rows[1]["parent_id"]=json!(SUB);f.rows[1]["name_encrypted"]=json!(beebeeb_core::encrypt::encrypt_name_for(&item_key(),"renamed.txt",None,beebeeb_core::chunk_stream::ExpectedChunkFormat::V2).unwrap());}ok(sync(&h,&d).await);bytes(&h,&d,"Nested/renamed.txt");assert!(!d.join("guest.txt").exists());repeats(&h,&d).await;let edited=b"owner local replacement preserves the random key";std::fs::write(d.join("Nested/renamed.txt"),edited).unwrap();ok(sync(&h,&d).await);let f=h.state.lock().unwrap();assert_eq!(f.upload_request["file_id"],FILE);assert_eq!(f.upload_request["base_version_number"],1);let mut dec=beebeeb_core::chunk_stream::ChunkDecryptor::for_push_with_file_key(item_key(),FILE,beebeeb_core::chunk_stream::ExpectedChunkFormat::V2);let mut plain=Vec::new();for c in &f.uploaded_chunks{plain.extend(dec.push_frame(c).unwrap().data);}dec.finish().unwrap();assert_eq!(plain,edited);drop(f);assert_eq!(h.count("DELETE",&format!("/api/v1/files/{FILE}")),0);assert_eq!(h.count("POST",&format!("/api/v1/files/{FILE}/trash")),0);repeats(&h,&d).await;h.close().await;}
#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn trash_restore_preserves_local_dirty(){let mut f=Fixture::transfer(false);f.rows.push(row(SUB,Some(ROOT),"keep",true,false));let h=Harness::new(f).await;let d=dir(&h);ok(sync(&h,&d).await);bytes(&h,&d,"guest.txt");let before=b"offline dirty content";std::fs::write(d.join("guest.txt"),before).unwrap();h.state.lock().unwrap().rows[1]["is_trashed"]=json!(true);let _=sync(&h,&d).await;let retained_dirty_bytes=std::fs::read(d.join("guest.txt")).unwrap();assert_eq!(retained_dirty_bytes,before);assert_eq!(h.count("POST","/api/v1/uploads/init"),0);h.state.lock().unwrap().rows[1]["is_trashed"]=json!(false);ok(sync(&h,&d).await);assert_eq!(std::fs::read(d.join("guest.txt")).unwrap(),before);h.close().await;}
#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn bad_key_keeps_listing_unhealthy(){let h=Harness::new(Fixture::transfer(false)).await;let d=dir(&h);ok(sync(&h,&d).await);bytes(&h,&d,"guest.txt");let before=b"preserve this local edit";std::fs::write(d.join("guest.txt"),before).unwrap();{let mut f=h.state.lock().unwrap();f.rows[1]["owner_wrapped_file_key"]=json!(B64.encode([1;60]));f.inventory[0]["encrypted_file_key"]=json!(B64.encode([2;60]));f.rows.push(row(SUB,Some(ROOT),"healthy",true,false));}let r=sync(&h,&d).await;assert_eq!(r.status.code(),Some(3));assert_eq!(std::fs::read(d.join("guest.txt")).unwrap(),before);assert!(state(&d)["files"]["guest.txt"].is_object());assert_eq!(h.count("POST","/api/v1/uploads/init"),0);assert!(!d.join(".bb-trash").exists());assert!(d.join("healthy").is_dir());h.close().await;}
#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn dry_run_has_zero_mutations(){let h=Harness::new(Fixture::transfer(false)).await;let d=dir(&h);ok(h.run(&["sync",d.to_str().unwrap(),"/Shared","--once","--dry-run"]).await);let dry_run_mutations=h.state.lock().unwrap().requests.iter().filter(|(m,_,_)|["POST","PATCH","PUT","DELETE"].contains(&m.as_str())).count();assert_eq!(dry_run_mutations,0);assert!(!d.join("guest.txt").exists());assert!(!d.join(".bb-sync.json").exists());assert!(h.count("GET",&format!("/api/v1/shared-folders/{ROOT}/keys"))>0);h.close().await;}
#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn adoption_timeout_then_winner(){let mut f=Fixture::transfer(false);f.adopt_failures=1;let h=Harness::new(f).await;let d=dir(&h);ok(sync(&h,&d).await);bytes(&h,&d,"guest.txt");let saved=state(&d);assert_eq!(saved["account_id"],ACCOUNT);assert!(saved["owned_keys"][FILE]["pending_owner_wrap"].as_array().is_some_and(|v|v.len()==60));{let mut f=h.state.lock().unwrap();f.custody_revoked=true;f.inventory.clear();f.adopt_lost=true;}ok(sync(&h,&d).await);bytes(&h,&d,"guest.txt");assert!(state(&d)["owned_keys"][FILE]["pending_owner_wrap"].is_null());assert_eq!(h.count("POST",&format!("/api/v1/files/{FILE}/owner-key")),2);repeats(&h,&d).await;h.close().await;}
#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn index_cas_retry_and_repair(){let mut f=Fixture::transfer(false);for sh in SearchIndex::build(&[(BAD.into(),"unrelated.txt".into())],64).encrypt_shards(&master()).unwrap(){f.owner_index.insert((sh.bucket,sh.page),(1,sh.blob));}f.shared_index.insert((0,0),(1,vec![0;60]));f.index_conflicts=3;let h=Harness::new(f).await;let d=dir(&h);ok(sync(&h,&d).await);bytes(&h,&d,"guest.txt");let shared_cas_attempts=h.state.lock().unwrap().requests.iter().filter(|(m,p,_)|m=="PUT"&&p.starts_with(&format!("/api/v1/shared-folders/{ROOT}/search-index"))).count();assert!(shared_cas_attempts<=3);assert_eq!(shared_cas_attempts,3);assert_eq!(state(&d)["dirty_shared_roots"],json!([ROOT]));assert!(index(&h,false).query("unrelated").contains(BAD));ok(sync(&h,&d).await);assert!(index(&h,true).query("guest").contains(FILE));assert!(index(&h,false).query("guest").contains(FILE));assert!(state(&d)["dirty_shared_roots"].as_array().unwrap().is_empty());{let mut f=h.state.lock().unwrap();f.shared_index.insert((0,0),(100,vec![0;60]));f.index_partial=true;}let puts=h.count("PUT",&format!("/api/v1/shared-folders/{ROOT}/search-index"));let _=sync(&h,&d).await;assert_eq!(h.count("PUT",&format!("/api/v1/shared-folders/{ROOT}/search-index")),puts);h.state.lock().unwrap().index_partial=false;ok(sync(&h,&d).await);assert!(index(&h,true).query("guest").contains(FILE));repeats(&h,&d).await;h.close().await;}
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
