use codewhale_runtime::settings::*;

#[test]
fn default_settings_resolve_to_the_underwater_theme() {
    // The fresh-install default is the Underwater theme, end to end from
    // `Settings::default()` through theme resolution.
    let settings = Settings::default();
    assert_eq!(settings.theme, "underwater");
    let (name, id, theme) =
        codewhale_palette::resolve_theme_setting(&settings.theme, None).expect("default resolves");
    assert_eq!(id, codewhale_palette::ThemeId::Underwater);
    assert_eq!(name, "underwater");
    assert_eq!(theme.name, "underwater");
    let saved: Settings = toml::from_str("theme = \"shoreline\"\n").expect("saved theme");
    assert_eq!(
        saved.theme, "shoreline",
        "upgrades preserve an explicit choice"
    );
}
