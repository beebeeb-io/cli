//! Task 4 of the `--parallel` plan (spec §5.1): the interrupt every wait of a push selects on.

use std::time::Duration;

use super::Interrupt;

#[tokio::test(start_paused = true)]
async fn interrupt_wait_never_misses_a_set() {
    let already = Interrupt::new();
    already.set();
    tokio::time::timeout(Duration::from_millis(1), already.wait())
        .await
        .expect("a set before the wait returns at once");

    let later = Interrupt::new();
    let waiter = {
        let later = later.clone();
        tokio::spawn(async move { later.wait().await })
    };
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(!waiter.is_finished(), "nothing set yet: the wait holds");
    later.set();
    tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .expect("a set from another task wakes the wait")
        .expect("task");
    assert!(later.is_set() && later.flag().load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test(start_paused = true)]
async fn every_clone_sees_the_one_interrupt() {
    let a = Interrupt::new();
    let b = a.clone();
    assert!(!b.is_set());
    a.set();
    assert!(b.is_set());
    tokio::time::timeout(Duration::from_millis(1), b.wait())
        .await
        .expect("clone wakes");
}
