pub const WHALE_NAME_MAX_CHARS: usize = 32;

pub const WHALE_COLORS: &[(&str, [u8; 3])] = &[
    ("scout", [0x35, 0x94, 0xd8]),
    ("patch", [0x54, 0xbb, 0xc5]),
    ("harbor", [0x78, 0x96, 0xdf]),
    ("echo", [0x43, 0x9e, 0xbd]),
    ("keel", [0x76, 0x8c, 0xc8]),
    ("lantern", [0x67, 0x98, 0xd0]),
    ("violet", [0xa7, 0x8b, 0xfa]),
    ("amber", [0xfb, 0xbf, 0x24]),
];

fn hides_text(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}'
        )
}

pub fn normalize_whale_name(text: &str) -> Result<Option<String>, String> {
    let name = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return Ok(None);
    }
    if name.chars().any(hides_text) {
        return Err(
            "whale_name cannot contain control or invisible formatting characters".to_string(),
        );
    }
    let count = name.chars().count();
    if count > WHALE_NAME_MAX_CHARS {
        return Err(format!(
            "whale_name is {count} characters; the limit is {WHALE_NAME_MAX_CHARS}"
        ));
    }
    Ok(Some(name))
}

pub fn normalize_whale_color(value: &str) -> Result<Option<String>, String> {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("default") {
        return Ok(None);
    }
    let lower = value.to_ascii_lowercase();
    if WHALE_COLORS.iter().any(|(name, _)| *name == lower) {
        return Ok(Some(lower));
    }
    if let Some([r, g, b]) = parse_hex(&lower) {
        return Ok(Some(format!("#{r:02x}{g:02x}{b:02x}")));
    }
    let names = WHALE_COLORS
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "invalid whale_color '{value}'. Expected one of {names}, a #rrggbb hex value, or default."
    ))
}

pub fn whale_color_rgb(value: &str) -> Option<[u8; 3]> {
    let lower = value.trim().to_ascii_lowercase();
    WHALE_COLORS
        .iter()
        .find(|(name, _)| *name == lower)
        .map(|(_, rgb)| *rgb)
        .or_else(|| parse_hex(&lower))
}

fn parse_hex(value: &str) -> Option<[u8; 3]> {
    let hex = value.strip_prefix('#').unwrap_or(value);
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).ok();
    Some([channel(0)?, channel(2)?, channel(4)?])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_collapse_whitespace_and_reject_hidden_or_long_text() {
        assert_eq!(
            normalize_whale_name("  Moby   Dick ").unwrap().as_deref(),
            Some("Moby Dick")
        );
        assert_eq!(normalize_whale_name("   ").unwrap(), None);
        assert_eq!(
            normalize_whale_name(&"鲸".repeat(32)).unwrap(),
            Some("鲸".repeat(32))
        );
        let too_long = normalize_whale_name(&"x".repeat(33)).unwrap_err();
        assert!(too_long.contains("limit is 32"), "{too_long}");
        for hidden in ["\u{1b}[31m", "Mo\u{202e}by", "a\u{200b}b", "a\u{7f}"] {
            assert!(normalize_whale_name(hidden).is_err(), "{hidden:?}");
        }
    }

    #[test]
    fn colors_accept_the_named_palette_and_hex_only() {
        assert_eq!(
            normalize_whale_color(" Harbor ").unwrap().as_deref(),
            Some("harbor")
        );
        assert_eq!(
            normalize_whale_color("#7FB2E0").unwrap().as_deref(),
            Some("#7fb2e0")
        );
        assert_eq!(
            normalize_whale_color("000000").unwrap().as_deref(),
            Some("#000000")
        );
        assert_eq!(normalize_whale_color("DEFAULT").unwrap(), None);
        assert_eq!(normalize_whale_color("").unwrap(), None);
        for rejected in [
            "blue",
            "#12345",
            "#12345g",
            "#1234567",
            "+12345",
            "rgb(1,2,3)",
        ] {
            let error = normalize_whale_color(rejected).unwrap_err();
            assert!(error.contains("scout, patch"), "{rejected}: {error}");
        }
        assert_eq!(whale_color_rgb("amber"), Some([0xfb, 0xbf, 0x24]));
        assert_eq!(whale_color_rgb("#0a141e"), Some([10, 20, 30]));
        assert_eq!(whale_color_rgb("nope"), None);
    }
}
