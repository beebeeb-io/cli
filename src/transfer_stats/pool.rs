//! Which storage pool a file's bytes went to, as far as a user may be told
//! (spec §4.4): the pool's id and its country. Nothing else from the server's
//! answer is kept, so nothing else can be printed.

/// The pool of one file. Every field is optional: an older server, or a
/// download (whose file rows carry no pool id), answers with less.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolLabel {
    pub id: Option<uuid::Uuid>,
    pub country: Option<String>,
    pub country_code: Option<String>,
}

impl PoolLabel {
    /// Read the pool from a server answer (an upload init or completion, or a
    /// file row). Only `storage_pool_id` and `storage_location.country` /
    /// `.country_code` are read: never `region` (a datacenter code), never the
    /// deprecated `storage_location.city` (Guus, LD-0021: countries, never
    /// cities). `None` when the answer names no pool at all.
    pub(crate) fn from_answer(answer: &serde_json::Value) -> Option<PoolLabel> {
        let id = answer
            .get("storage_pool_id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<uuid::Uuid>().ok());
        let location = answer.get("storage_location");
        let text = |key: &str| {
            location
                .and_then(|l| l.get(key))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        let (country, country_code) = (text("country"), text("country_code"));
        if id.is_none() && country.is_none() && country_code.is_none() {
            return None;
        }
        Some(PoolLabel {
            id,
            country,
            country_code,
        })
    }

    /// The first 8 hex digits of the pool id (the form `bb ls` uses for file ids).
    pub(crate) fn id8(&self) -> Option<String> {
        self.id.map(|id| id.simple().to_string()[..8].to_string())
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
    fn pool_label_keeps_only_id_and_country() {
        let label = PoolLabel::from_answer(&answer()).expect("a label");
        let shown = format!("{label:?} {}", label.id8().unwrap());
        let mut never = vec!["zz9", "Testville", "objects.storage.test", "Storageco", "storageco"];
        never.extend(PROVIDER_NAMES);
        assert_absent(&shown, &never);
        assert!(shown.contains("Germany") && shown.contains("DE"), "{shown}");
        assert_eq!(label.id8().as_deref(), Some("3e15382b"));
    }

    #[test]
    fn pool_label_without_an_id_keeps_the_country() {
        let row = serde_json::json!({
            "id": "file-1",
            "storage_location": {"region": "europe", "country": "Finland", "country_code": "FI", "city": "Testville"}
        });
        let label = PoolLabel::from_answer(&row).expect("a label");
        assert_eq!(label.id, None);
        assert_eq!(label.country.as_deref(), Some("Finland"));
        assert_eq!(label.id8(), None);
        assert_eq!(
            PoolLabel::from_answer(&serde_json::json!({"id": "x"})),
            None,
            "nothing to label"
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
