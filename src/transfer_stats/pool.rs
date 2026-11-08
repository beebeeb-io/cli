//! Where a file's bytes are stored, as far as a user may be told (spec §4.4;
//! Guus and the lead, 2026-11-06): the storage country. Nothing else from the
//! server's answer is kept, so nothing else can be printed or put in `--json`.

/// The storage country of one file's pool. Both fields are optional: an older
/// server answers with less.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolLabel {
    pub country: Option<String>,
    pub country_code: Option<String>,
}

impl PoolLabel {
    /// Read the storage country from a server answer (an upload init or
    /// completion, or a file row). Only `storage_location.country` and
    /// `.country_code` are read: never `storage_pool_id` (no meaning to a user),
    /// never `region` (it can be a datacenter code), never the deprecated
    /// `storage_location.city` (Guus, LD-0021: countries, never cities). `None`
    /// when the answer names no country.
    pub(crate) fn from_answer(answer: &serde_json::Value) -> Option<PoolLabel> {
        let location = answer.get("storage_location");
        let text = |key: &str| {
            location
                .and_then(|l| l.get(key))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        let (country, country_code) = (text("country"), text("country_code"));
        if country.is_none() && country_code.is_none() {
            return None;
        }
        Some(PoolLabel { country, country_code })
    }
}

/// The shared absence check of the hard-limit tests (spec §9.4): nothing a
/// user is shown may contain a provider name, a storage host, a city or a
/// secret. The provider names are the claims guard's list (workspace
/// `scripts/claims-guard/policy.conf`, `PROVIDER_NAMES`), lowercase.
#[cfg(test)]
pub(crate) mod absent {
    pub(crate) const PROVIDER_NAMES: [&str; 8] = [
        "hetzner",
        "leaseweb",
        "ovhcloud",
        "scaleway",
        "contabo",
        "vultr",
        "digitalocean",
        "your-objectstorage.com",
    ];

    /// Panics naming the first needle found in `haystack` (compared lowercase).
    pub(crate) fn assert_absent(haystack: &str, needles: &[&str]) {
        let hay = haystack.to_lowercase();
        for needle in needles {
            assert!(
                !hay.contains(&needle.to_lowercase()),
                "must never be shown: {needle:?} found in {haystack:?}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::absent::{PROVIDER_NAMES, assert_absent};
    use super::*;

    /// An answer as the server shapes it, plus every field a pool row has that a
    /// user must never see. Invented values only (spec §9.4 "Fixture values").
    fn answer() -> serde_json::Value {
        serde_json::json!({
            "storage_pool_id": "3e15382b-0000-4000-8000-000000000001",
            "region": "zz9",
            "storage_location": {
                "region": "europe",
                "country": "Germany",
                "country_code": "DE",
                "city": "Testville"
            },
            "name": "zz9-pool",
            "display_name": "Storageco Testville",
            "endpoint": "https://objects.storage.test",
            "provider": "storageco"
        })
    }

    #[test]
    fn pool_label_keeps_only_the_country() {
        let label = PoolLabel::from_answer(&answer()).expect("a label");
        let shown = format!("{label:?}");
        let mut never = vec![
            "3e15382b",
            "zz9",
            "Testville",
            "objects.storage.test",
            "Storageco",
            "storageco",
        ];
        never.extend(PROVIDER_NAMES);
        assert_absent(&shown, &never);
        assert_eq!(
            label,
            PoolLabel {
                country: Some("Germany".into()),
                country_code: Some("DE".into())
            }
        );
    }

    #[test]
    fn a_pool_id_without_a_country_is_no_label() {
        let row = serde_json::json!({
            "id": "file-1",
            "storage_location": {"region": "europe", "country": "Finland", "country_code": "FI", "city": "Testville"}
        });
        let label = PoolLabel::from_answer(&row).expect("a label");
        assert_eq!(label.country.as_deref(), Some("Finland"));
        assert_eq!(
            PoolLabel::from_answer(&serde_json::json!({"storage_pool_id": "3e15382b-0000-4000-8000-000000000001"})),
            None,
            "an id alone names no country: nothing is shown, never a guess"
        );
    }

    #[test]
    #[should_panic(expected = "must never be shown")]
    fn negative_tests_can_fail() {
        let planted = "chunk 1/1 · https://objects.storage.test/b/k?X-Amz-Signature=abc · Hetzner";
        let mut never = vec!["X-Amz-Signature=abc"];
        never.extend(PROVIDER_NAMES);
        assert_absent(planted, &never);
    }
}
