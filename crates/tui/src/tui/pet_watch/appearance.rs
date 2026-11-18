//! Presentation preferences never enter the simulation or its replay journal.
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BodyStyle {
    #[default]
    Smooth,
    Pixel,
}
impl BodyStyle {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "smooth" => Some(Self::Smooth),
            "pixel" => Some(Self::Pixel),
            _ => None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Appearance {
    pub background: [u8; 3],
    pub background_top: [u8; 3],
    pub particle: [u8; 3],
    pub event_colors: bool,
    pub brightness: f64,
    pub dot_scale: f64,
    pub glow: f64,
    pub environment: bool,
    #[serde(default)]
    pub body_style: BodyStyle,
    #[serde(default, rename = "textLanguage", skip_serializing)]
    legacy_text_language: serde::de::IgnoredAny,
}
impl Default for Appearance {
    fn default() -> Self {
        Self {
            background: [8, 15, 21],
            background_top: [24, 45, 56],
            particle: [135, 221, 219],
            event_colors: true,
            brightness: 1.25,
            dot_scale: 1.0,
            glow: 0.5,
            environment: true,
            body_style: BodyStyle::default(),
            legacy_text_language: serde::de::IgnoredAny,
        }
    }
}
impl Appearance {
    pub fn valid(&self) -> bool {
        self.brightness.is_finite()
            && (0.25..=2.0).contains(&self.brightness)
            && self.dot_scale.is_finite()
            && (0.65..=1.8).contains(&self.dot_scale)
            && self.glow.is_finite()
            && (0.0..=1.0).contains(&self.glow)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appearance_saved_with_a_text_language_still_loads_and_drops_it() {
        let mut saved = serde_json::to_value(Appearance::default()).unwrap();
        saved["textLanguage"] = serde_json::json!("ja");
        let loaded: Appearance = serde_json::from_value(saved).unwrap();
        assert_eq!(loaded, Appearance::default());
        assert!(
            serde_json::to_value(&loaded)
                .unwrap()
                .get("textLanguage")
                .is_none()
        );
        let mut unknown = serde_json::to_value(Appearance::default()).unwrap();
        unknown["fontSize"] = serde_json::json!(3);
        assert!(serde_json::from_value::<Appearance>(unknown).is_err());
    }
}
