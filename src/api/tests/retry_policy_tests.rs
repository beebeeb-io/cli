use super::CHUNK_RETRY;

/// The retry vector: the ladder the CLI has always slept, now read from
/// core's policy. 200, 400, 800, 1600, 3200 ms for attempts 1..=5, and no
/// wait at all before the first attempt.
#[test]
fn chunk_retry_ladder_matches_the_shipped_one() {
    let ladder: Vec<u64> = (0..=CHUNK_RETRY.max_attempts).map(|retry| CHUNK_RETRY.delay_ms(retry)).collect();
    assert_eq!(ladder, vec![0, 200, 400, 800, 1_600, 3_200]);
    assert_eq!(CHUNK_RETRY.max_attempts, 5);
}
