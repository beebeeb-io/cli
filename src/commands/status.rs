//! `bb status`: an alias for `bb whoami`.

pub async fn run() -> Result<(), String> {
    super::whoami::run().await
}
