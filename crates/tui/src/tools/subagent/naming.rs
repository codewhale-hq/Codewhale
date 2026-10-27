//! Whale nickname pools and locale-aware display-name helpers for sub-agents.

/// Whale species used as friendly names for sub-agents in the UI. The full
/// Cetacea infraorder — baleen whales (Mysticeti), toothed whales
/// (Odontoceti), plus select dolphin species (family Delphinidae) that
/// don't conflate with existing agent type labels. Porpoises (Phocoenidae)
/// are excluded because their name doesn't carry well as a friendly label.
///
/// English and Simplified-Chinese names are stored as adjacent pairs. Name
/// selection follows the active session locale; it never mixes languages in
/// one session. Smaller curated pools below cover every other shipped locale.
///
/// Taxonomy source: Society for Marine Mammalogy (2025).
pub const WHALE_NICKNAMES: &[&str] = &[
    "Blue",
    "蓝鲸",
    "Humpback",
    "座头鲸",
    "Sperm",
    "抹香鲸",
    "Fin",
    "长须鲸",
    "Sei",
    "塞鲸",
    "Bryde's",
    "布氏鲸",
    "Minke",
    "小须鲸",
    "Antarctic Minke",
    "南极小须鲸",
    "Pygmy Right",
    "小露脊鲸",
    "Omura's",
    "大村鲸",
    "Eden's",
    "艾氏鲸",
    "Rice's",
    "赖斯鲸",
    "Gray",
    "灰鲸",
    "Bowhead",
    "弓头鲸",
    "North Atlantic Right",
    "北大西洋露脊鲸",
    "North Pacific Right",
    "北太平洋露脊鲸",
    "Southern Right",
    "南露脊鲸",
    "Beluga",
    "白鲸",
    "Narwhal",
    "独角鲸",
    "Orca",
    "虎鲸",
    "Pilot",
    "领航鲸",
    "False Killer",
    "伪虎鲸",
    "Pygmy Killer",
    "小虎鲸",
    "Melon-headed",
    "瓜头鲸",
    "Beaked",
    "喙鲸",
    "Cuvier's Beaked",
    "柯氏喙鲸",
    "Baird's Beaked",
    "贝氏喙鲸",
    "Blainville's Beaked",
    "柏氏喙鲸",
    "Ginkgo-toothed Beaked",
    "银杏齿喙鲸",
    "Strap-toothed",
    "带齿喙鲸",
    "Stejneger's Beaked",
    "斯氏喙鲸",
    "Dwarf Sperm",
    "小抹香鲸",
    "Pygmy Sperm",
    "侏儒抹香鲸",
    "Rough-toothed",
    "糙齿海豚",
    "Atlantic Spotted",
    "大西洋斑海豚",
    "Pantropical Spotted",
    "热带斑海豚",
    "Spinner",
    "长吻飞旋海豚",
    "Clymene",
    "短吻飞旋海豚",
    "Striped",
    "条纹海豚",
    "Common Bottlenose",
    "宽吻海豚",
    "Indo-Pacific Bottlenose",
    "印太瓶鼻海豚",
    "Risso's",
    "灰海豚",
    "Commerson's",
    "花斑海豚",
    "Chilean",
    "智利海豚",
    "Heaviside's",
    "海氏矮海豚",
    "Hector's",
    "赫氏矮海豚",
    "Amazon River",
    "亚马逊河豚",
    "Ganges River",
    "恒河豚",
    "Indus River",
    "印度河豚",
    "La Plata",
    "拉普拉塔河豚",
    "Franciscana",
    "拉河豚",
];

pub(super) const WHALE_NICKNAMES_JA: &[&str] = &[
    "シロナガスクジラ",
    "ザトウクジラ",
    "マッコウクジラ",
    "ナガスクジラ",
    "イワシクジラ",
    "ミンククジラ",
    "コククジラ",
    "ホッキョククジラ",
    "シロイルカ",
    "イッカク",
    "シャチ",
    "ゴンドウクジラ",
];

pub(super) const WHALE_NICKNAMES_ZH_HANT: &[&str] = &[
    "藍鯨",
    "座頭鯨",
    "抹香鯨",
    "長鬚鯨",
    "塞鯨",
    "布氏鯨",
    "小鬚鯨",
    "灰鯨",
    "弓頭鯨",
    "白鯨",
    "獨角鯨",
    "虎鯨",
];

pub(super) const WHALE_NICKNAMES_PT_BR: &[&str] = &[
    "Azul",
    "Jubarte",
    "Cachalote",
    "Baleia-fin",
    "Baleia-sei",
    "Baleia-de-bryde",
    "Baleia-minke",
    "Cinzenta",
    "Baleia-franca",
    "Beluga",
    "Narval",
    "Orca",
];

pub(super) const WHALE_NICKNAMES_ES_419: &[&str] = &[
    "Azul",
    "Jorobada",
    "Cachalote",
    "Rorcual común",
    "Rorcual sei",
    "Rorcual de Bryde",
    "Rorcual aliblanco",
    "Gris",
    "Ballena franca",
    "Beluga",
    "Narval",
    "Orca",
];

pub(super) const WHALE_NICKNAMES_VI: &[&str] = &[
    "Cá voi xanh",
    "Cá voi lưng gù",
    "Cá nhà táng",
    "Cá voi vây",
    "Cá voi Sei",
    "Cá voi Bryde",
    "Cá voi Minke",
    "Cá voi xám",
    "Cá voi đầu cong",
    "Cá voi trắng",
    "Kỳ lân biển",
    "Cá voi sát thủ",
];

pub(super) const WHALE_NICKNAMES_KO: &[&str] = &[
    "대왕고래",
    "혹등고래",
    "향유고래",
    "참고래",
    "보리고래",
    "브라이드고래",
    "밍크고래",
    "귀신고래",
    "북극고래",
    "흰고래",
    "외뿔고래",
    "범고래",
];

pub(super) const WHALE_NICKNAMES_CA: &[&str] = &[
    "Balena blava",
    "Balena geperuda",
    "Catxalot",
    "Rorcual comú",
    "Rorcual boreal",
    "Balena minke",
    "Balena franca",
    "Balena de Groenlàndia",
    "Beluga",
    "Narval",
    "Orca",
    "Calderó",
];

pub(super) const WHALE_NICKNAMES_DE: &[&str] = &[
    "Blauwal",
    "Buckelwal",
    "Pottwal",
    "Finnwal",
    "Seiwal",
    "Zwergwal",
    "Glattwal",
    "Grönlandwal",
    "Beluga",
    "Narwal",
    "Orca",
    "Pilotwal",
];

pub(super) const WHALE_NICKNAMES_FR: &[&str] = &[
    "Baleine bleue",
    "Baleine à bosse",
    "Cachalot",
    "Rorqual commun",
    "Rorqual boréal",
    "Petit rorqual",
    "Baleine franche",
    "Baleine boréale",
    "Béluga",
    "Narval",
    "Orque",
    "Globicéphale",
];

pub(super) const WHALE_NICKNAMES_ID: &[&str] = &[
    "Paus biru",
    "Paus bungkuk",
    "Paus sperma",
    "Paus sirip",
    "Paus sei",
    "Paus minke",
    "Paus sikat",
    "Paus bowhead",
    "Beluga",
    "Narwal",
    "Orca",
    "Paus pilot",
];

pub(super) const WHALE_NICKNAMES_HI: &[&str] = &[
    "नीली व्हेल",
    "कूबड़ व्हेल",
    "स्पर्म व्हेल",
    "फिन व्हेल",
    "सेई व्हेल",
    "मिंक व्हेल",
    "राइट व्हेल",
    "बोहेड व्हेल",
    "बेलुगा",
    "नार्व्हल",
    "ओर्का",
    "पायलट व्हेल",
];

pub(super) const WHALE_NICKNAMES_RU: &[&str] = &[
    "Синий кит",
    "Горбатый кит",
    "Кашалот",
    "Финвал",
    "Сейвал",
    "Малый полосатик",
    "Гладкий кит",
    "Гренландский кит",
    "Белуха",
    "Нарвал",
    "Косатка",
    "Гринда",
];

pub(super) const WHALE_NICKNAMES_UK: &[&str] = &[
    "Синій кит",
    "Горбатий кит",
    "Кашалот",
    "Фінвал",
    "Сейвал",
    "Малий смугач",
    "Гладкий кит",
    "Гренландський кит",
    "Белуга",
    "Нарвал",
    "Косатка",
    "Гринда",
];

/// Return a deterministic whale name in the active UI locale.
#[must_use]
pub fn whale_name_for_id_in_locale(id: &str, locale_tag: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut hasher);
    let hash = hasher.finish() as usize;
    let normalized = locale_tag.trim().to_ascii_lowercase();

    let localized_pool = match normalized.as_str() {
        "ja" => Some(WHALE_NICKNAMES_JA),
        "zh-hant" => Some(WHALE_NICKNAMES_ZH_HANT),
        "pt-br" => Some(WHALE_NICKNAMES_PT_BR),
        "es-419" => Some(WHALE_NICKNAMES_ES_419),
        "vi" => Some(WHALE_NICKNAMES_VI),
        "ko" => Some(WHALE_NICKNAMES_KO),
        "ca" => Some(WHALE_NICKNAMES_CA),
        "de" => Some(WHALE_NICKNAMES_DE),
        "fr" => Some(WHALE_NICKNAMES_FR),
        "id" => Some(WHALE_NICKNAMES_ID),
        "hi" => Some(WHALE_NICKNAMES_HI),
        "ru" => Some(WHALE_NICKNAMES_RU),
        "uk" => Some(WHALE_NICKNAMES_UK),
        _ => None,
    };
    if let Some(pool) = localized_pool {
        return pool[hash % pool.len()].to_string();
    }

    debug_assert_eq!(WHALE_NICKNAMES.len() % 2, 0);
    let pair_count = WHALE_NICKNAMES.len() / 2;
    let pair = hash % pair_count;
    let language_offset = usize::from(normalized == "zh-hans");
    let idx = pair * 2 + language_offset;
    WHALE_NICKNAMES[idx].to_string()
}

/// Assign a unique locale-matched whale name for an agent ID.
/// If the deterministic name is taken, appends a numeric suffix (for example,
/// `Orca (2)`).
#[must_use]
pub fn assign_unique_whale_name_in_locale(
    id: &str,
    active_names: &std::collections::HashSet<String>,
    locale_tag: &str,
) -> String {
    let base = whale_name_for_id_in_locale(id, locale_tag);
    if !active_names.contains(&base) {
        return base;
    }
    // Deterministic suffix from the same hash to keep it stable
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut hasher);
    let suffix_seed = hasher.finish();
    for i in 2.. {
        let candidate = format!("{base} ({i})");
        if !active_names.contains(&candidate) {
            return candidate;
        }
        // Vary the probe using the seed
        let probe = (suffix_seed.wrapping_add(i as u64)) % 100;
        let candidate2 = format!("{base} ({probe})");
        if !active_names.contains(&candidate2) {
            return candidate2;
        }
    }
    // Fallback (should never reach here)
    format!("{base} ({})", id.get(..4).unwrap_or("?"))
}

/// Return the unsuffixed whale label when `name` could have been generated for
/// this exact agent id in a shipped locale. Numeric collision suffixes are
/// presentation-only and do not make the label user-authored.
pub(super) fn generated_whale_name_base<'a>(agent_id: &str, name: &'a str) -> Option<&'a str> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let base = name
        .rsplit_once(" (")
        .and_then(|(base, suffix)| {
            suffix
                .strip_suffix(')')
                .filter(|number| !number.is_empty() && number.chars().all(|ch| ch.is_ascii_digit()))
                .map(|_| base)
        })
        .unwrap_or(name);

    // With no persisted provenance bit, the narrowest truthful test is whether
    // this exact agent id could have generated the label in a shipped locale.
    // A user-authored label that happens to be a whale word for some other id
    // remains explicit. An exact deterministic match is inherently ambiguous
    // and stays classified as generated for backward compatibility.
    codewhale_localization::Locale::shipped()
        .iter()
        .any(|locale| whale_name_for_id_in_locale(agent_id, locale.tag()) == base)
        .then_some(base)
}

/// Derive the generated whale labels shown for a set of workers from their
/// locale-neutral ids and the active UI language.
///
/// Persisted `nickname` values predate locale-scoped naming and may contain a
/// whale label chosen under another language. Those generated values are
/// deliberately ignored here. A nickname that this agent id could not have
/// generated is an explicit custom label and remains intact, even when it is a
/// whale word from a built-in pool.
#[must_use]
pub(crate) fn localized_whale_display_names<'a>(
    agents: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
    locale_tag: &str,
) -> std::collections::HashMap<String, String> {
    let mut by_id = std::collections::BTreeMap::<String, Option<String>>::new();
    for (agent_id, nickname) in agents {
        if agent_id.trim().is_empty() {
            continue;
        }
        let nickname = nickname
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string);
        by_id
            .entry(agent_id.to_string())
            .and_modify(|existing| {
                if existing.is_none() && nickname.is_some() {
                    *existing = nickname.clone();
                }
            })
            .or_insert(nickname);
    }

    let mut names = std::collections::HashMap::with_capacity(by_id.len());
    let mut active_names = std::collections::HashSet::new();

    // Reserve explicit labels first so generated names never shadow them.
    for (agent_id, nickname) in &by_id {
        let Some(nickname) = nickname
            .as_deref()
            .filter(|name| generated_whale_name_base(agent_id, name).is_none())
        else {
            continue;
        };
        active_names.insert(nickname.to_string());
        names.insert(agent_id.clone(), nickname.to_string());
    }

    // BTreeMap iteration makes collision suffix ownership stable even when
    // manager/progress event order changes between frames or session loads.
    for agent_id in by_id.keys() {
        if names.contains_key(agent_id) {
            continue;
        }
        let name = assign_unique_whale_name_in_locale(agent_id, &active_names, locale_tag);
        active_names.insert(name.clone());
        names.insert(agent_id.clone(), name);
    }

    names
}

/// A nickname someone chose for this agent: a workflow task label or a
/// user-given name. A whale name generated from the agent id is presentation
/// only and is not an explicit name.
pub(crate) fn explicit_nickname<'a>(agent_id: &str, nickname: Option<&'a str>) -> Option<&'a str> {
    nickname
        .map(str::trim)
        .filter(|name| !name.is_empty() && *name != agent_id)
        .filter(|name| generated_whale_name_base(agent_id, name).is_none())
}

/// The role word for an agent, most specific first: the resolved profile, the
/// assignment role, the requested profile, the route's canonical role, then
/// the Fleet type, in its public spelling.
pub(crate) fn subagent_role_label(
    child_route: Option<&super::ChildRouteReceipt>,
    assignment_role: Option<&str>,
    agent_type: &super::FleetRole,
) -> String {
    let non_empty = |value: &str| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_string())
    };
    child_route
        .and_then(|route| route.resolved_profile_id.as_deref())
        .and_then(non_empty)
        .or_else(|| assignment_role.and_then(non_empty))
        .or_else(|| {
            child_route
                .and_then(|route| route.requested_profile.as_deref())
                .and_then(non_empty)
        })
        .or_else(|| child_route.and_then(|route| non_empty(&route.canonical_role)))
        .or_else(|| non_empty(agent_type.as_str()))
        .map_or_else(
            || "agent".to_string(),
            |role| codewhale_agent_contract::role::public_role_label(&role),
        )
}

/// The one name an agent goes by, on every surface and every host (#6565).
///
/// An explicit nickname (the workflow's task label, or a name someone gave the
/// agent) wins as it is. A dispatch (session) name comes next, followed by the
/// role when the name does not already say it. An agent with neither goes by
/// its role. It is never the raw agent id.
pub(crate) fn subagent_display_name(
    agent_id: &str,
    nickname: Option<&str>,
    session_name: Option<&str>,
    role: &str,
) -> String {
    if let Some(explicit) = explicit_nickname(agent_id, nickname) {
        return explicit.to_string();
    }
    match session_name
        .map(str::trim)
        .filter(|name| !name.is_empty() && *name != agent_id)
    {
        Some(name) if name.contains(role) => name.to_string(),
        Some(name) => format!("{name} · {role}"),
        None => role.to_string(),
    }
}

/// [`subagent_display_name`] for a manager snapshot.
pub(crate) fn subagent_result_display_name(result: &super::SubAgentResult) -> String {
    let role = subagent_role_label(
        result.child_route.as_ref(),
        result.assignment.role.as_deref(),
        &result.agent_type,
    );
    subagent_display_name(
        &result.agent_id,
        result.nickname.as_deref(),
        Some(result.name.as_str()),
        &role,
    )
}

#[cfg(test)]
mod display_name_tests {
    use super::*;
    use codewhale_agent_contract::role::FleetRole;

    #[test]
    fn one_name_precedence_never_falls_to_the_agent_id() {
        let id = "agent_1a2b3c4d";
        // A workflow task label (an explicit nickname) wins as it is.
        assert_eq!(
            subagent_display_name(id, Some("audit docs"), Some("triage"), "explore"),
            "audit docs"
        );
        // A generated whale name is not an explicit name.
        let whale = whale_name_for_id_in_locale(id, "en");
        assert_eq!(
            subagent_display_name(id, Some(&whale), Some("triage"), "explore"),
            "triage · explore"
        );
        // The dispatch name carries the role unless it already says it.
        assert_eq!(
            subagent_display_name(id, None, Some("explore-docs"), "explore"),
            "explore-docs"
        );
        // The manager seeds the session name with the id: that is no name.
        assert_eq!(
            subagent_display_name(id, None, Some(id), "explore"),
            "explore"
        );
        assert_eq!(
            subagent_display_name(id, Some(" "), None, "review"),
            "review"
        );
    }

    #[test]
    fn role_label_prefers_the_most_specific_token() {
        assert_eq!(
            subagent_role_label(None, Some("reviewer"), &FleetRole::Worker),
            "reviewer"
        );
        assert_eq!(
            subagent_role_label(None, None, &FleetRole::Builder),
            "implement"
        );
        assert_eq!(
            subagent_role_label(None, Some("  "), &FleetRole::Scout),
            "explore"
        );
    }
}
