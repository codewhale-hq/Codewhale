// Generated from Codewhale design 5.3.0; sha256 0d9acc771562a7b50fc9473e23ebccbff7cbc56962639277943a0cb0131ea83e. Do not edit.
const PAYLOAD = {
 "version": "5.3.0",
 "textStyles": [
  {
   "name": "CW 5/Caption",
   "family": "Geist",
   "style": "Regular",
   "size": 11.5,
   "line": 16
  },
  {
   "name": "CW 5/Label",
   "family": "Geist",
   "style": "Regular",
   "size": 12.5,
   "line": 18
  },
  {
   "name": "CW 5/Control",
   "family": "Geist",
   "style": "Regular",
   "size": 13,
   "line": 18
  },
  {
   "name": "CW 5/Body",
   "family": "Geist",
   "style": "Regular",
   "size": 14,
   "line": 22
  },
  {
   "name": "CW 5/Prose",
   "family": "Geist",
   "style": "Regular",
   "size": 16,
   "line": 26
  },
  {
   "name": "CW 5/Heading",
   "family": "Geist",
   "style": "SemiBold",
   "size": 15,
   "line": 24
  },
  {
   "name": "CW 5/Title",
   "family": "Geist",
   "style": "SemiBold",
   "size": 22,
   "line": 28
  },
  {
   "name": "CW 5/Display",
   "family": "Geist",
   "style": "SemiBold",
   "size": 28,
   "line": 34
  },
  {
   "name": "CW 5/Code",
   "family": "Geist Mono",
   "style": "Regular",
   "size": 13,
   "line": 20
  }
 ],
 "collections": [
  {
   "name": "CW 5 \u00b7 Color",
   "modes": [
    "Dark",
    "Light"
   ],
   "variables": [
    {
     "name": "outside",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "var(--outside)",
     "values": {
      "Dark": {
       "hex": "0d121b",
       "alpha": 1
      },
      "Light": {
       "hex": "f9fafc",
       "alpha": 1
      }
     }
    },
    {
     "name": "surface",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "var(--surface)",
     "values": {
      "Dark": {
       "hex": "030508",
       "alpha": 1
      },
      "Light": {
       "hex": "f1f5f9",
       "alpha": 1
      }
     }
    },
    {
     "name": "panel",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "var(--panel)",
     "values": {
      "Dark": {
       "hex": "04070c",
       "alpha": 1
      },
      "Light": {
       "hex": "fbfcfd",
       "alpha": 1
      }
     }
    },
    {
     "name": "text",
     "type": "COLOR",
     "scopes": [
      "SHAPE_FILL",
      "TEXT_FILL",
      "STROKE_COLOR"
     ],
     "web": "var(--text)",
     "values": {
      "Dark": {
       "hex": "e8edf2",
       "alpha": 1
      },
      "Light": {
       "hex": "1b2a35",
       "alpha": 1
      }
     }
    },
    {
     "name": "muted",
     "type": "COLOR",
     "scopes": [
      "SHAPE_FILL",
      "TEXT_FILL",
      "STROKE_COLOR"
     ],
     "web": "var(--muted)",
     "values": {
      "Dark": {
       "hex": "a8b6c3",
       "alpha": 1
      },
      "Light": {
       "hex": "4a545d",
       "alpha": 1
      }
     }
    },
    {
     "name": "disabled",
     "type": "COLOR",
     "scopes": [
      "SHAPE_FILL",
      "TEXT_FILL",
      "STROKE_COLOR"
     ],
     "web": "var(--disabled)",
     "values": {
      "Dark": {
       "hex": "848583",
       "alpha": 1
      },
      "Light": {
       "hex": "6f7780",
       "alpha": 1
      }
     }
    },
    {
     "name": "line",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL",
      "STROKE_COLOR"
     ],
     "web": "var(--line)",
     "values": {
      "Dark": {
       "hex": "3b3c3f",
       "alpha": 1
      },
      "Light": {
       "hex": "d5dce3",
       "alpha": 1
      }
     }
    },
    {
     "name": "accent",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL",
      "TEXT_FILL",
      "STROKE_COLOR"
     ],
     "web": "var(--accent)",
     "values": {
      "Dark": {
       "hex": "8fbdff",
       "alpha": 1
      },
      "Light": {
       "hex": "245bc7",
       "alpha": 1
      }
     }
    },
    {
     "name": "button",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "var(--button)",
     "values": {
      "Dark": {
       "hex": "0b48bb",
       "alpha": 1
      },
      "Light": {
       "hex": "0b48bb",
       "alpha": 1
      }
     }
    },
    {
     "name": "button-text",
     "type": "COLOR",
     "scopes": [
      "SHAPE_FILL",
      "TEXT_FILL",
      "STROKE_COLOR"
     ],
     "web": "var(--button-text)",
     "values": {
      "Dark": {
       "hex": "ffffff",
       "alpha": 1
      },
      "Light": {
       "hex": "ffffff",
       "alpha": 1
      }
     }
    },
    {
     "name": "attention",
     "type": "COLOR",
     "scopes": [
      "SHAPE_FILL",
      "TEXT_FILL",
      "STROKE_COLOR"
     ],
     "web": "var(--attention)",
     "values": {
      "Dark": {
       "hex": "eab982",
       "alpha": 1
      },
      "Light": {
       "hex": "84530d",
       "alpha": 1
      }
     }
    },
    {
     "name": "hover",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "var(--hover)",
     "values": {
      "Dark": {
       "hex": "303134",
       "alpha": 1
      },
      "Light": {
       "hex": "e3e9ef",
       "alpha": 1
      }
     }
    },
    {
     "name": "selected",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "var(--selected)",
     "values": {
      "Dark": {
       "hex": "17191c",
       "alpha": 1
      },
      "Light": {
       "hex": "d8e0e8",
       "alpha": 1
      }
     }
    },
    {
     "name": "live",
     "type": "COLOR",
     "scopes": [
      "SHAPE_FILL",
      "TEXT_FILL",
      "STROKE_COLOR"
     ],
     "web": "var(--live)",
     "values": {
      "Dark": {
       "hex": "9ecbbd",
       "alpha": 1
      },
      "Light": {
       "hex": "396554",
       "alpha": 1
      }
     }
    },
    {
     "name": "danger",
     "type": "COLOR",
     "scopes": [
      "SHAPE_FILL",
      "TEXT_FILL",
      "STROKE_COLOR"
     ],
     "web": "var(--danger)",
     "values": {
      "Dark": {
       "hex": "eea59d",
       "alpha": 1
      },
      "Light": {
       "hex": "9e3f36",
       "alpha": 1
      }
     }
    },
    {
     "name": "border-strong",
     "type": "COLOR",
     "scopes": [
      "STROKE_COLOR"
     ],
     "web": "var(--border-strong)",
     "values": {
      "Dark": {
       "hex": "828386",
       "alpha": 1
      },
      "Light": {
       "hex": "737d88",
       "alpha": 1
      }
     }
    },
    {
     "name": "ocean-depth",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "var(--ocean-depth)",
     "values": {
      "Dark": {
       "hex": "202123",
       "alpha": 1
      },
      "Light": {
       "hex": "f6f9fc",
       "alpha": 1
      }
     }
    },
    {
     "name": "diff-add",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "var(--diff-add)",
     "values": {
      "Dark": {
       "hex": "22392f",
       "alpha": 1
      },
      "Light": {
       "hex": "d2eadd",
       "alpha": 1
      }
     }
    },
    {
     "name": "diff-remove",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "var(--diff-remove)",
     "values": {
      "Dark": {
       "hex": "45302f",
       "alpha": 1
      },
      "Light": {
       "hex": "f8e1dd",
       "alpha": 1
      }
     }
    },
    {
     "name": "card/fill",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "var(--card-fill)",
     "values": {
      "Dark": {
       "hex": "ffffff",
       "alpha": 0.035
      },
      "Light": {
       "hex": "ffffff",
       "alpha": 1
      }
     }
    },
    {
     "name": "card/border",
     "type": "COLOR",
     "scopes": [
      "STROKE_COLOR"
     ],
     "web": "var(--card-border)",
     "values": {
      "Dark": {
       "hex": "ffffff",
       "alpha": 0.08
      },
      "Light": {
       "hex": "1f3340",
       "alpha": 0.1
      }
     }
    },
    {
     "name": "sheet/fill",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "var(--sheet-fill)",
     "values": {
      "Dark": {
       "hex": "05080d",
       "alpha": 0.74
      },
      "Light": {
       "hex": "fcfdfe",
       "alpha": 0.88
      }
     }
    },
    {
     "name": "sheet/border",
     "type": "COLOR",
     "scopes": [
      "STROKE_COLOR"
     ],
     "web": "var(--sheet-border)",
     "values": {
      "Dark": {
       "hex": "ffffff",
       "alpha": 0.05
      },
      "Light": {
       "hex": "1f3340",
       "alpha": 0.1
      }
     }
    },
    {
     "name": "glass/clear-fill",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "var(--glass-clear-fill)",
     "values": {
      "Dark": {
       "hex": "141c28",
       "alpha": 0.4
      },
      "Light": {
       "hex": "ffffff",
       "alpha": 0.4
      }
     }
    },
    {
     "name": "glass/frosted-fill",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "var(--glass-frosted-fill)",
     "values": {
      "Dark": {
       "hex": "141c28",
       "alpha": 0.58
      },
      "Light": {
       "hex": "ffffff",
       "alpha": 0.55
      }
     }
    },
    {
     "name": "glass/solid-fill",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "var(--glass-solid-fill)",
     "values": {
      "Dark": {
       "hex": "141c28",
       "alpha": 1
      },
      "Light": {
       "hex": "ffffff",
       "alpha": 1
      }
     }
    },
    {
     "name": "glass/border",
     "type": "COLOR",
     "scopes": [
      "STROKE_COLOR"
     ],
     "web": "var(--glass-border)",
     "values": {
      "Dark": {
       "hex": "ffffff",
       "alpha": 0.09
      },
      "Light": {
       "hex": "ffffff",
       "alpha": 0.75
      }
     }
    },
    {
     "name": "glass/highlight",
     "type": "COLOR",
     "scopes": [
      "STROKE_COLOR"
     ],
     "web": "var(--glass-highlight)",
     "values": {
      "Dark": {
       "hex": "ffffff",
       "alpha": 0.14
      },
      "Light": {
       "hex": "ffffff",
       "alpha": 0.75
      }
     }
    },
    {
     "name": "ombre/rise",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "var(--ombre-rise)",
     "values": {
      "Dark": {
       "hex": "1775c9",
       "alpha": 1
      },
      "Light": {
       "hex": "1775c9",
       "alpha": 1
      }
     }
    },
    {
     "name": "ombre/deep",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "var(--ombre-deep)",
     "values": {
      "Dark": {
       "hex": "0b48bb",
       "alpha": 1
      },
      "Light": {
       "hex": "0b48bb",
       "alpha": 1
      }
     }
    },
    {
     "name": "water/top",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL"
     ],
     "web": "var(--water) \u00b7 top stop",
     "values": {
      "Dark": {
       "hex": "0b1e3a",
       "alpha": 1
      },
      "Light": {
       "hex": "bfe1f2",
       "alpha": 1
      }
     }
    },
    {
     "name": "water/upper",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL"
     ],
     "web": "var(--water) \u00b7 upper stop",
     "values": {
      "Dark": {
       "hex": "071326",
       "alpha": 1
      },
      "Light": {
       "hex": "dceef3",
       "alpha": 1
      }
     }
    },
    {
     "name": "water/lower",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL"
     ],
     "web": "var(--water) \u00b7 lower stop",
     "values": {
      "Dark": {
       "hex": "04090f",
       "alpha": 1
      },
      "Light": {
       "hex": "e7f0f6",
       "alpha": 1
      }
     }
    },
    {
     "name": "water/bottom",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL"
     ],
     "web": "var(--water) \u00b7 bottom stop",
     "values": {
      "Dark": {
       "hex": "030508",
       "alpha": 1
      },
      "Light": {
       "hex": "f1f5f9",
       "alpha": 1
      }
     }
    },
    {
     "name": "surface-light/peak",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL"
     ],
     "web": "var(--surface-light) \u00b7 peak stop",
     "values": {
      "Dark": {
       "hex": "2896e6",
       "alpha": 0.32
      },
      "Light": {
       "hex": "ffffff",
       "alpha": 0.85
      }
     }
    },
    {
     "name": "surface-light/mid",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL"
     ],
     "web": "var(--surface-light) \u00b7 mid stop",
     "values": {
      "Dark": {
       "hex": "0b48bb",
       "alpha": 0.1
      },
      "Light": {
       "hex": "ffffff",
       "alpha": 0.25
      }
     }
    },
    {
     "name": "surface-light/edge",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL"
     ],
     "web": "var(--surface-light) \u00b7 edge stop",
     "values": {
      "Dark": {
       "hex": "0b48bb",
       "alpha": 0
      },
      "Light": {
       "hex": "ffffff",
       "alpha": 0
      }
     }
    },
    {
     "name": "shadow/popover",
     "type": "COLOR",
     "scopes": [
      "EFFECT_COLOR"
     ],
     "web": "var(--elevation-popover)",
     "values": {
      "Dark": {
       "hex": "000000",
       "alpha": 0.4
      },
      "Light": {
       "hex": "000000",
       "alpha": 0.16
      }
     }
    },
    {
     "name": "shadow/dialog",
     "type": "COLOR",
     "scopes": [
      "EFFECT_COLOR"
     ],
     "web": "var(--elevation-dialog)",
     "values": {
      "Dark": {
       "hex": "000000",
       "alpha": 0.5
      },
      "Light": {
       "hex": "000000",
       "alpha": 0.2
      }
     }
    },
    {
     "name": "effort/fill",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "var(--tint-accent-fill)",
     "values": {
      "Dark": {
       "hex": "8fbdff",
       "alpha": 0.16
      },
      "Light": {
       "hex": "245bc7",
       "alpha": 0.16
      }
     }
    },
    {
     "name": "chip/accent-border",
     "type": "COLOR",
     "scopes": [
      "STROKE_COLOR"
     ],
     "web": "var(--tint-accent-border)",
     "values": {
      "Dark": {
       "hex": "8fbdff",
       "alpha": 0.35
      },
      "Light": {
       "hex": "245bc7",
       "alpha": 0.35
      }
     }
    },
    {
     "name": "count/fill",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "var(--tint-count-fill)",
     "values": {
      "Dark": {
       "hex": "e8edf2",
       "alpha": 0.1
      },
      "Light": {
       "hex": "1b2a35",
       "alpha": 0.1
      }
     }
    },
    {
     "name": "tint/danger-border",
     "type": "COLOR",
     "scopes": [
      "STROKE_COLOR"
     ],
     "web": "var(--tint-danger-border)",
     "values": {
      "Dark": {
       "hex": "eea59d",
       "alpha": 0.45
      },
      "Light": {
       "hex": "9e3f36",
       "alpha": 0.45
      }
     }
    },
    {
     "name": "tint/scrim",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "var(--tint-scrim)",
     "values": {
      "Dark": {
       "hex": "030508",
       "alpha": 0.45
      },
      "Light": {
       "hex": "f1f5f9",
       "alpha": 0.45
      }
     }
    },
    {
     "name": "kbd/on-primary-border",
     "type": "COLOR",
     "scopes": [
      "STROKE_COLOR"
     ],
     "web": "var(--tint-on-primary-border)",
     "values": {
      "Dark": {
       "hex": "ffffff",
       "alpha": 0.45
      },
      "Light": {
       "hex": "ffffff",
       "alpha": 0.45
      }
     }
    },
    {
     "name": "whale/rise",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "whale/source/spec.json palette",
     "values": {
      "Dark": {
       "hex": "3594d8",
       "alpha": 1
      },
      "Light": {
       "hex": "1e8fd8",
       "alpha": 1
      }
     }
    },
    {
     "name": "whale/deep",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "whale/source/spec.json palette",
     "values": {
      "Dark": {
       "hex": "2b70d5",
       "alpha": 1
      },
      "Light": {
       "hex": "0b48bb",
       "alpha": 1
      }
     }
    },
    {
     "name": "whale/pouch",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "whale/source/spec.json palette",
     "values": {
      "Dark": {
       "hex": "dcecf8",
       "alpha": 1
      },
      "Light": {
       "hex": "dcebf7",
       "alpha": 1
      }
     }
    },
    {
     "name": "whale/eye",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "whale/source/spec.json palette",
     "values": {
      "Dark": {
       "hex": "0b1424",
       "alpha": 1
      },
      "Light": {
       "hex": "0b1424",
       "alpha": 1
      }
     }
    }
   ]
  },
  {
   "name": "CW 5 \u00b7 Metrics",
   "modes": [
    "Value"
   ],
   "variables": [
    {
     "name": "spacing/hairline",
     "type": "FLOAT",
     "scopes": [
      "GAP"
     ],
     "web": "var(--spacing-hairline)",
     "values": {
      "Value": 2
     }
    },
    {
     "name": "spacing/compact",
     "type": "FLOAT",
     "scopes": [
      "GAP"
     ],
     "web": "var(--spacing-compact)",
     "values": {
      "Value": 4
     }
    },
    {
     "name": "spacing/tight",
     "type": "FLOAT",
     "scopes": [
      "GAP"
     ],
     "web": "var(--spacing-tight)",
     "values": {
      "Value": 6
     }
    },
    {
     "name": "spacing/control",
     "type": "FLOAT",
     "scopes": [
      "GAP"
     ],
     "web": "var(--spacing-control)",
     "values": {
      "Value": 8
     }
    },
    {
     "name": "spacing/snug",
     "type": "FLOAT",
     "scopes": [
      "GAP"
     ],
     "web": "var(--spacing-snug)",
     "values": {
      "Value": 10
     }
    },
    {
     "name": "spacing/group",
     "type": "FLOAT",
     "scopes": [
      "GAP"
     ],
     "web": "var(--spacing-group)",
     "values": {
      "Value": 12
     }
    },
    {
     "name": "spacing/section",
     "type": "FLOAT",
     "scopes": [
      "GAP"
     ],
     "web": "var(--spacing-section)",
     "values": {
      "Value": 16
     }
    },
    {
     "name": "spacing/large",
     "type": "FLOAT",
     "scopes": [
      "GAP"
     ],
     "web": "var(--spacing-large)",
     "values": {
      "Value": 24
     }
    },
    {
     "name": "spacing/page",
     "type": "FLOAT",
     "scopes": [
      "GAP"
     ],
     "web": "var(--spacing-page)",
     "values": {
      "Value": 32
     }
    },
    {
     "name": "radius/small",
     "type": "FLOAT",
     "scopes": [
      "CORNER_RADIUS"
     ],
     "web": "var(--radius-small)",
     "values": {
      "Value": 6
     }
    },
    {
     "name": "radius/field",
     "type": "FLOAT",
     "scopes": [
      "CORNER_RADIUS"
     ],
     "web": "var(--radius-field)",
     "values": {
      "Value": 8
     }
    },
    {
     "name": "radius/control",
     "type": "FLOAT",
     "scopes": [
      "CORNER_RADIUS"
     ],
     "web": "var(--radius-control)",
     "values": {
      "Value": 10
     }
    },
    {
     "name": "radius/panel",
     "type": "FLOAT",
     "scopes": [
      "CORNER_RADIUS"
     ],
     "web": "var(--radius-panel)",
     "values": {
      "Value": 18
     }
    },
    {
     "name": "radius/sheet",
     "type": "FLOAT",
     "scopes": [
      "CORNER_RADIUS"
     ],
     "web": "var(--radius-sheet)",
     "values": {
      "Value": 18
     }
    },
    {
     "name": "radius/window",
     "type": "FLOAT",
     "scopes": [
      "CORNER_RADIUS"
     ],
     "web": "var(--radius-window)",
     "values": {
      "Value": 14
     }
    },
    {
     "name": "radius/card",
     "type": "FLOAT",
     "scopes": [
      "CORNER_RADIUS"
     ],
     "web": "var(--radius-card)",
     "values": {
      "Value": 14
     }
    },
    {
     "name": "radius/composer",
     "type": "FLOAT",
     "scopes": [
      "CORNER_RADIUS"
     ],
     "web": "var(--radius-composer)",
     "values": {
      "Value": 24
     }
    },
    {
     "name": "radius/chip",
     "type": "FLOAT",
     "scopes": [
      "CORNER_RADIUS"
     ],
     "web": "var(--radius-chip)",
     "values": {
      "Value": 999
     }
    },
    {
     "name": "size/control",
     "type": "FLOAT",
     "scopes": [
      "WIDTH_HEIGHT"
     ],
     "web": "var(--size-control)",
     "values": {
      "Value": 28
     }
    },
    {
     "name": "size/row",
     "type": "FLOAT",
     "scopes": [
      "WIDTH_HEIGHT"
     ],
     "web": "var(--size-row)",
     "values": {
      "Value": 30
     }
    },
    {
     "name": "size/touch-target",
     "type": "FLOAT",
     "scopes": [
      "WIDTH_HEIGHT"
     ],
     "web": "var(--size-touch-target)",
     "values": {
      "Value": 44
     }
    },
    {
     "name": "size/icon",
     "type": "FLOAT",
     "scopes": [
      "WIDTH_HEIGHT"
     ],
     "web": "var(--size-icon)",
     "values": {
      "Value": 16
     }
    },
    {
     "name": "size/icon-large",
     "type": "FLOAT",
     "scopes": [
      "WIDTH_HEIGHT"
     ],
     "web": "var(--size-icon-large)",
     "values": {
      "Value": 20
     }
    },
    {
     "name": "size/sidebar",
     "type": "FLOAT",
     "scopes": [
      "WIDTH_HEIGHT"
     ],
     "web": "var(--size-sidebar)",
     "values": {
      "Value": 272
     }
    },
    {
     "name": "size/panel",
     "type": "FLOAT",
     "scopes": [
      "WIDTH_HEIGHT"
     ],
     "web": "var(--size-panel)",
     "values": {
      "Value": 300
     }
    },
    {
     "name": "focus/width",
     "type": "FLOAT",
     "scopes": [
      "STROKE_FLOAT"
     ],
     "web": "var(--focus-width)",
     "values": {
      "Value": 2
     }
    },
    {
     "name": "focus/offset",
     "type": "FLOAT",
     "scopes": [
      "GAP"
     ],
     "web": "var(--focus-offset)",
     "values": {
      "Value": 3
     }
    }
   ]
  }
 ]
};
const DRY = typeof DRY_RUN !== "undefined" && DRY_RUN;
const rgba = (v) => {
  const n = parseInt(v.hex, 16);
  return { r: ((n >> 16) & 255) / 255, g: ((n >> 8) & 255) / 255, b: (n & 255) / 255, a: v.alpha };
};
const same = (a, b) =>
  typeof a === "number" || typeof b === "number"
    ? a === b
    : !!a && !!b && ["r", "g", "b", "a"].every((k) => Math.abs((a[k] ?? 1) - (b[k] ?? 1)) < 0.002);
const report = { version: PAYLOAD.version, dry: DRY, created: [], updated: [], unchanged: 0, notInTokens: [], styles: { created: [], updated: [], unchanged: 0 } };
const collections = await figma.variables.getLocalVariableCollectionsAsync();
const variables = await figma.variables.getLocalVariablesAsync();
for (const wanted of PAYLOAD.collections) {
  let collection = collections.find((c) => c.name === wanted.name);
  if (!collection) {
    if (DRY) {
      report.created.push(...wanted.variables.map((v) => `${wanted.name}: ${v.name}`));
      continue;
    }
    collection = figma.variables.createVariableCollection(wanted.name);
    collection.renameMode(collection.modes[0].modeId, wanted.modes[0]);
    for (const mode of wanted.modes.slice(1)) collection.addMode(mode);
  }
  const modeId = Object.fromEntries(collection.modes.map((m) => [m.name, m.modeId]));
  const missingMode = wanted.modes.find((m) => !modeId[m]);
  if (missingMode) throw new Error(`${wanted.name} has no mode named ${missingMode}`);
  const existing = variables.filter((v) => v.variableCollectionId === collection.id);
  const names = new Set(wanted.variables.map((v) => v.name));
  report.notInTokens.push(...existing.filter((v) => !names.has(v.name)).map((v) => `${wanted.name}: ${v.name}`));
  for (const token of wanted.variables) {
    let variable = existing.find((v) => v.name === token.name);
    if (!variable) {
      report.created.push(`${wanted.name}: ${token.name}`);
      if (DRY) continue;
      variable = figma.variables.createVariable(token.name, collection, token.type);
      variable.scopes = token.scopes;
      variable.setVariableCodeSyntax("WEB", token.web);
    } else if (!DRY && (variable.codeSyntax.WEB || "") !== token.web) {
      variable.setVariableCodeSyntax("WEB", token.web);
    }
    let changed = false;
    for (const mode of wanted.modes) {
      const next = token.type === "COLOR" ? rgba(token.values[mode]) : token.values[mode];
      if (same(variable.valuesByMode[modeId[mode]], next)) continue;
      changed = true;
      if (!DRY) variable.setValueForMode(modeId[mode], next);
    }
    if (changed && !report.created.includes(`${wanted.name}: ${token.name}`)) report.updated.push(`${wanted.name}: ${token.name}`);
    if (!changed) report.unchanged += 1;
  }
}
const textStyles = await figma.getLocalTextStylesAsync();
for (const wanted of PAYLOAD.textStyles) {
  const font = { family: wanted.family, style: wanted.style };
  let style = textStyles.find((s) => s.name === wanted.name);
  const matches =
    style &&
    style.fontName.family === font.family &&
    style.fontName.style === font.style &&
    style.fontSize === wanted.size &&
    style.lineHeight.unit === "PIXELS" &&
    style.lineHeight.value === wanted.line;
  if (matches) {
    report.styles.unchanged += 1;
    continue;
  }
  report.styles[style ? "updated" : "created"].push(wanted.name);
  if (DRY) continue;
  await figma.loadFontAsync(font);
  if (!style) {
    style = figma.createTextStyle();
    style.name = wanted.name;
  }
  style.fontName = font;
  style.fontSize = wanted.size;
  style.lineHeight = { unit: "PIXELS", value: wanted.line };
}
return report;
