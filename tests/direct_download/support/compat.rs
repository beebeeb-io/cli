//! One behaviour group of the same integration suite.

use super::*;

#[test]
fn the_pull_json_and_help_stay_compatible() {
    let env = setup(Discovery::Offer, Grants::Normal, &[], &[]);
    let (out, json) = pull(&env, &[], &[]);
    assert!(out.status.success(), "{}", describe(&out));
    for key in [
        "id",
        "name",
        "size_bytes",
        "download_speed_bps",
        "elapsed_ms",
        "output",
        "transport",
    ] {
        assert!(json.get(key).is_some(), "missing {key} in {json}");
    }
    assert_eq!(json["size_bytes"], PLAIN_LEN);
    let help = bb(&env, &["pull", "--help"], &[]);
    assert!(help.status.success(), "{}", describe(&help));
    assert!(
        !String::from_utf8_lossy(&help.stdout).contains("--transport"),
        "the debug switch is hidden from --help:\n{}",
        describe(&help)
    );
    let _ = &env.api_url;
}
