pub(super) struct BundledFile {
    pub(super) path: &'static str,
    pub(super) bytes: &'static [u8],
}

pub(super) struct CreatorSkill {
    pub(super) name: &'static str,
    pub(super) files: &'static [BundledFile],
    pub(super) host_notes: Option<&'static str>,
}

pub(super) const CREATOR_SKILLS: &[CreatorSkill] = &[
    CreatorSkill {
        name: "agent-experience",
        files: &[BundledFile {
            path: "LICENSE",
            bytes: include_bytes!("../../assets/dsh-creator/LICENSE"),
        }],
        host_notes: None,
    },
    CreatorSkill {
        name: "cordis-composition-reference",
        files: &[
            BundledFile {
                path: "LICENSE",
                bytes: include_bytes!("../../assets/dsh-creator/LICENSE"),
            },
            BundledFile {
                path: "references/packages.md",
                bytes: include_bytes!(
                    "../../assets/dsh-creator/skills/cordis-composition-reference/references/packages.md"
                ),
            },
        ],
        host_notes: Some(include_str!(
            "../../assets/dsh-creator/host-notes/cordis-composition-reference.md"
        )),
    },
    CreatorSkill {
        name: "cordis-plugin-development",
        files: &[
            BundledFile {
                path: "LICENSE",
                bytes: include_bytes!("../../assets/dsh-creator/LICENSE"),
            },
            BundledFile {
                path: "references/host-plugin.md",
                bytes: include_bytes!(
                    "../../assets/dsh-creator/skills/cordis-plugin-development/references/host-plugin.md"
                ),
            },
            BundledFile {
                path: "references/mcp-bundle.md",
                bytes: include_bytes!(
                    "../../assets/dsh-creator/skills/cordis-plugin-development/references/mcp-bundle.md"
                ),
            },
            BundledFile {
                path: "references/practices.md",
                bytes: include_bytes!(
                    "../../assets/dsh-creator/skills/cordis-plugin-development/references/practices.md"
                ),
            },
            BundledFile {
                path: "references/ui-plugin.md",
                bytes: include_bytes!(
                    "../../assets/dsh-creator/skills/cordis-plugin-development/references/ui-plugin.md"
                ),
            },
            BundledFile {
                path: "references/user-actions.md",
                bytes: include_bytes!(
                    "../../assets/dsh-creator/skills/cordis-plugin-development/references/user-actions.md"
                ),
            },
            BundledFile {
                path: "references/verification.md",
                bytes: include_bytes!(
                    "../../assets/dsh-creator/skills/cordis-plugin-development/references/verification.md"
                ),
            },
            BundledFile {
                path: "templates/decoration/client.js",
                bytes: include_bytes!(
                    "../../assets/dsh-creator/skills/cordis-plugin-development/templates/decoration/client.js"
                ),
            },
            BundledFile {
                path: "templates/decoration/cordis.patch.yml",
                bytes: include_bytes!(
                    "../../assets/dsh-creator/skills/cordis-plugin-development/templates/decoration/cordis.patch.yml"
                ),
            },
            BundledFile {
                path: "templates/decoration/icon.svg",
                bytes: include_bytes!(
                    "../../assets/dsh-creator/skills/cordis-plugin-development/templates/decoration/icon.svg"
                ),
            },
            BundledFile {
                path: "templates/decoration/index.js",
                bytes: include_bytes!(
                    "../../assets/dsh-creator/skills/cordis-plugin-development/templates/decoration/index.js"
                ),
            },
            BundledFile {
                path: "templates/decoration/locale/de.json",
                bytes: include_bytes!(
                    "../../assets/dsh-creator/skills/cordis-plugin-development/templates/decoration/locale/de.json"
                ),
            },
            BundledFile {
                path: "templates/decoration/locale/en.json",
                bytes: include_bytes!(
                    "../../assets/dsh-creator/skills/cordis-plugin-development/templates/decoration/locale/en.json"
                ),
            },
            BundledFile {
                path: "templates/decoration/locale/es.json",
                bytes: include_bytes!(
                    "../../assets/dsh-creator/skills/cordis-plugin-development/templates/decoration/locale/es.json"
                ),
            },
            BundledFile {
                path: "templates/decoration/locale/fr.json",
                bytes: include_bytes!(
                    "../../assets/dsh-creator/skills/cordis-plugin-development/templates/decoration/locale/fr.json"
                ),
            },
            BundledFile {
                path: "templates/decoration/locale/ja.json",
                bytes: include_bytes!(
                    "../../assets/dsh-creator/skills/cordis-plugin-development/templates/decoration/locale/ja.json"
                ),
            },
            BundledFile {
                path: "templates/decoration/locale/pt.json",
                bytes: include_bytes!(
                    "../../assets/dsh-creator/skills/cordis-plugin-development/templates/decoration/locale/pt.json"
                ),
            },
            BundledFile {
                path: "templates/decoration/locale/ru.json",
                bytes: include_bytes!(
                    "../../assets/dsh-creator/skills/cordis-plugin-development/templates/decoration/locale/ru.json"
                ),
            },
            BundledFile {
                path: "templates/decoration/package.json",
                bytes: include_bytes!(
                    "../../assets/dsh-creator/skills/cordis-plugin-development/templates/decoration/package.json"
                ),
            },
            BundledFile {
                path: "templates/mcp/cordis.patch.yml",
                bytes: include_bytes!(
                    "../../assets/dsh-creator/skills/cordis-plugin-development/templates/mcp/cordis.patch.yml"
                ),
            },
            BundledFile {
                path: "templates/mcp/icon.svg",
                bytes: include_bytes!(
                    "../../assets/dsh-creator/skills/cordis-plugin-development/templates/mcp/icon.svg"
                ),
            },
            BundledFile {
                path: "templates/mcp/locale/de.json",
                bytes: include_bytes!(
                    "../../assets/dsh-creator/skills/cordis-plugin-development/templates/mcp/locale/de.json"
                ),
            },
            BundledFile {
                path: "templates/mcp/locale/en.json",
                bytes: include_bytes!(
                    "../../assets/dsh-creator/skills/cordis-plugin-development/templates/mcp/locale/en.json"
                ),
            },
            BundledFile {
                path: "templates/mcp/locale/es.json",
                bytes: include_bytes!(
                    "../../assets/dsh-creator/skills/cordis-plugin-development/templates/mcp/locale/es.json"
                ),
            },
            BundledFile {
                path: "templates/mcp/locale/fr.json",
                bytes: include_bytes!(
                    "../../assets/dsh-creator/skills/cordis-plugin-development/templates/mcp/locale/fr.json"
                ),
            },
            BundledFile {
                path: "templates/mcp/locale/ja.json",
                bytes: include_bytes!(
                    "../../assets/dsh-creator/skills/cordis-plugin-development/templates/mcp/locale/ja.json"
                ),
            },
            BundledFile {
                path: "templates/mcp/locale/pt.json",
                bytes: include_bytes!(
                    "../../assets/dsh-creator/skills/cordis-plugin-development/templates/mcp/locale/pt.json"
                ),
            },
            BundledFile {
                path: "templates/mcp/locale/ru.json",
                bytes: include_bytes!(
                    "../../assets/dsh-creator/skills/cordis-plugin-development/templates/mcp/locale/ru.json"
                ),
            },
            BundledFile {
                path: "templates/mcp/package.json",
                bytes: include_bytes!(
                    "../../assets/dsh-creator/skills/cordis-plugin-development/templates/mcp/package.json"
                ),
            },
        ],
        host_notes: Some(include_str!(
            "../../assets/dsh-creator/host-notes/cordis-plugin-development.md"
        )),
    },
    CreatorSkill {
        name: "editing-cordis-compositions",
        files: &[BundledFile {
            path: "LICENSE",
            bytes: include_bytes!("../../assets/dsh-creator/LICENSE"),
        }],
        host_notes: Some(include_str!(
            "../../assets/dsh-creator/host-notes/editing-cordis-compositions.md"
        )),
    },
];
