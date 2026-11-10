use codewhale_app_server_protocol::methods::{
    ClientNotificationMethod, ClientRequestMethod, ServerNotificationMethod, ServerRequestMethod,
    Tier,
};
use std::collections::{BTreeMap, BTreeSet};

const INVENTORY: &str = include_str!("../METHODS.md");

struct Kind {
    section: &'static str,
    pinned: &'static str,
    typed: Vec<(&'static str, Tier)>,
}

fn kinds() -> [Kind; 4] {
    fn typed<T: Copy>(
        all: &[T],
        method: fn(T) -> &'static str,
        tier: fn(T) -> Tier,
    ) -> Vec<(&'static str, Tier)> {
        all.iter()
            .map(|value| (method(*value), tier(*value)))
            .collect()
    }
    [
        Kind {
            section: "Client requests",
            pinned: include_str!("pinned/codex/ClientRequest.ts"),
            typed: typed(
                ClientRequestMethod::ALL,
                ClientRequestMethod::method,
                ClientRequestMethod::tier,
            ),
        },
        Kind {
            section: "Client notifications",
            pinned: include_str!("pinned/codex/ClientNotification.ts"),
            typed: typed(
                ClientNotificationMethod::ALL,
                ClientNotificationMethod::method,
                ClientNotificationMethod::tier,
            ),
        },
        Kind {
            section: "Server requests",
            pinned: include_str!("pinned/codex/ServerRequest.ts"),
            typed: typed(
                ServerRequestMethod::ALL,
                ServerRequestMethod::method,
                ServerRequestMethod::tier,
            ),
        },
        Kind {
            section: "Server notifications",
            pinned: include_str!("pinned/codex/ServerNotification.ts"),
            typed: typed(
                ServerNotificationMethod::ALL,
                ServerNotificationMethod::method,
                ServerNotificationMethod::tier,
            ),
        },
    ]
}

fn pinned_methods(source: &str) -> Vec<&str> {
    source
        .split("\"method\": \"")
        .skip(1)
        .map(|rest| rest.split('"').next().expect("method literal"))
        .collect()
}

fn inventory_rows(section: &str) -> Vec<(String, String, String)> {
    let heading = format!("## {section}\n");
    let start = INVENTORY
        .find(&heading)
        .unwrap_or_else(|| panic!("inventory has no section {section}"))
        + heading.len();
    INVENTORY[start..]
        .lines()
        .take_while(|line| !line.starts_with("## "))
        .filter(|line| line.starts_with("| `"))
        .map(|line| {
            let cells = line
                .trim_matches('|')
                .split('|')
                .map(str::trim)
                .collect::<Vec<_>>();
            assert_eq!(cells.len(), 5, "inventory row shape: {line}");
            (
                cells[0].trim_matches('`').to_owned(),
                cells[1].to_owned(),
                cells[2].to_owned(),
            )
        })
        .collect()
}

#[test]
fn typed_methods_inventory_and_pinned_export_agree() {
    let mut totals = BTreeMap::new();
    for kind in kinds() {
        let pinned = pinned_methods(kind.pinned);
        let pinned_set = pinned.iter().copied().collect::<BTreeSet<_>>();
        assert_eq!(
            pinned.len(),
            pinned_set.len(),
            "{}: duplicate pinned method",
            kind.section
        );

        let rows = inventory_rows(kind.section);
        let mut inventory = BTreeMap::new();
        for (method, tier, status) in &rows {
            assert!(
                matches!(tier.as_str(), "A" | "B" | "C" | "undecided"),
                "{}: {method} has tier {tier}",
                kind.section
            );
            assert!(
                matches!(
                    status.as_str(),
                    "implemented" | "partial" | "missing" | "method-not-found" | "never-sent"
                ),
                "{}: {method} has status {status}",
                kind.section
            );
            assert_eq!(
                tier == "C",
                matches!(status.as_str(), "method-not-found" | "never-sent"),
                "{}: {method} status {status} does not fit tier {tier}",
                kind.section
            );
            assert!(
                inventory.insert(method.as_str(), tier.as_str()).is_none(),
                "{}: {method} listed twice",
                kind.section
            );
            *totals.entry(tier.clone()).or_insert(0usize) += 1;
        }
        let inventory_set = inventory.keys().copied().collect::<BTreeSet<_>>();
        assert_eq!(
            pinned_set.difference(&inventory_set).collect::<Vec<_>>(),
            Vec::<&&str>::new(),
            "{}: pinned export names methods the inventory lacks",
            kind.section
        );
        assert_eq!(
            inventory_set.difference(&pinned_set).collect::<Vec<_>>(),
            Vec::<&&str>::new(),
            "{}: inventory names methods the pinned export lacks",
            kind.section
        );

        let mut typed = BTreeSet::new();
        for (method, tier) in &kind.typed {
            assert!(
                typed.insert(*method),
                "{}: {method} typed twice",
                kind.section
            );
            assert!(
                pinned_set.contains(method),
                "{}: typed {method} is not in the pinned export",
                kind.section
            );
            let expected = match tier {
                Tier::A => "A",
                Tier::B => "B",
            };
            assert_eq!(
                inventory.get(method).copied(),
                Some(expected),
                "{}: typed {method} disagrees with the inventory tier",
                kind.section
            );
        }
        for (method, tier) in &inventory {
            if matches!(*tier, "A" | "B") {
                assert!(
                    typed.contains(method),
                    "{}: Tier {tier} {method} has no typed variant",
                    kind.section
                );
            }
        }
    }
    assert!(totals.values().sum::<usize>() > 0);
}

#[test]
fn typed_methods_round_trip_their_wire_names() {
    for method in ClientRequestMethod::ALL {
        assert_eq!(
            ClientRequestMethod::from_method(method.method()),
            Some(*method)
        );
        let wire = serde_json::to_value(method).expect("serialize method");
        assert_eq!(wire, method.method());
        assert_eq!(
            serde_json::from_value::<ClientRequestMethod>(wire).expect("deserialize method"),
            *method
        );
    }
    for method in ServerNotificationMethod::ALL {
        assert_eq!(
            ServerNotificationMethod::from_method(method.method()),
            Some(*method)
        );
    }
    for method in ServerRequestMethod::ALL {
        assert_eq!(
            ServerRequestMethod::from_method(method.method()),
            Some(*method)
        );
    }
    assert_eq!(ClientRequestMethod::from_method("account/read"), None);
    assert!(serde_json::from_value::<ClientRequestMethod>("feedback/upload".into()).is_err());
}
