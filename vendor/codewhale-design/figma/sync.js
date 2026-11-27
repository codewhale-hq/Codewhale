// Generated from Codewhale design 5.5.0; sha256 b154b28c796626428f20b38db59ee6449aa876831ae04d4ddceb3cb1fce8185c. Do not edit.
const PAYLOAD = {
 "version": "5.5.0",
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
   "line": 20
  },
  {
   "name": "CW 5/Body",
   "family": "Geist",
   "style": "Regular",
   "size": 14,
   "line": 22
  },
  {
   "name": "CW 5/Reading",
   "family": "Geist",
   "style": "Regular",
   "size": 15,
   "line": 25
  },
  {
   "name": "CW 5/Prose",
   "family": "Geist",
   "style": "Regular",
   "size": 16,
   "line": 27
  },
  {
   "name": "CW 5/Heading",
   "family": "Geist",
   "style": "SemiBold",
   "size": 15,
   "line": 22
  },
  {
   "name": "CW 5/Subheading",
   "family": "Geist",
   "style": "SemiBold",
   "size": 17,
   "line": 26
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
       "hex": "0f131c",
       "alpha": 1
      },
      "Light": {
       "hex": "fcfdfe",
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
       "hex": "05070b",
       "alpha": 1
      },
      "Light": {
       "hex": "f4f7fc",
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
       "hex": "080b11",
       "alpha": 1
      },
      "Light": {
       "hex": "fefeff",
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
       "hex": "edf1f7",
       "alpha": 1
      },
      "Light": {
       "hex": "162230",
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
       "hex": "b2bfcd",
       "alpha": 1
      },
      "Light": {
       "hex": "45515f",
       "alpha": 1
      }
     }
    },
    {
     "name": "subagent",
     "type": "COLOR",
     "scopes": [
      "SHAPE_FILL",
      "TEXT_FILL",
      "STROKE_COLOR"
     ],
     "web": "var(--subagent)",
     "values": {
      "Dark": {
       "hex": "bccad9",
       "alpha": 1
      },
      "Light": {
       "hex": "374553",
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
       "hex": "828f9e",
       "alpha": 1
      },
      "Light": {
       "hex": "68727f",
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
       "hex": "263040",
       "alpha": 1
      },
      "Light": {
       "hex": "d5dde8",
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
       "hex": "94beff",
       "alpha": 1
      },
      "Light": {
       "hex": "1552c6",
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
       "hex": "0b4cc4",
       "alpha": 1
      },
      "Light": {
       "hex": "0b4cc4",
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
       "hex": "f0be82",
       "alpha": 1
      },
      "Light": {
       "hex": "7b4b06",
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
       "hex": "202938",
       "alpha": 1
      },
      "Light": {
       "hex": "e2eaf4",
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
       "hex": "18202e",
       "alpha": 1
      },
      "Light": {
       "hex": "d6e1f0",
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
       "hex": "7fdfc8",
       "alpha": 1
      },
      "Light": {
       "hex": "146857",
       "alpha": 1
      }
     }
    },
    {
     "name": "live-peak",
     "type": "COLOR",
     "scopes": [
      "SHAPE_FILL",
      "TEXT_FILL",
      "STROKE_COLOR"
     ],
     "web": "var(--live-peak)",
     "values": {
      "Dark": {
       "hex": "e3fcf4",
       "alpha": 1
      },
      "Light": {
       "hex": "0b3f33",
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
       "hex": "f4a8a0",
       "alpha": 1
      },
      "Light": {
       "hex": "a42531",
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
       "hex": "8a97a8",
       "alpha": 1
      },
      "Light": {
       "hex": "687381",
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
       "hex": "0b111c",
       "alpha": 1
      },
      "Light": {
       "hex": "f7f9fc",
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
       "hex": "1d352a",
       "alpha": 1
      },
      "Light": {
       "hex": "d2ebdd",
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
       "hex": "422b2c",
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
       "alpha": 0.04
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
       "alpha": 0.095
      },
      "Light": {
       "hex": "182838",
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
       "hex": "090c13",
       "alpha": 0.8
      },
      "Light": {
       "hex": "ffffff",
       "alpha": 0.92
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
       "alpha": 0.075
      },
      "Light": {
       "hex": "182838",
       "alpha": 0.09
      }
     }
    },
    {
     "name": "change-bar/fill",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "var(--change-bar-fill)",
     "values": {
      "Dark": {
       "hex": "ffffff",
       "alpha": 0.045
      },
      "Light": {
       "hex": "1b2a35",
       "alpha": 0.035
      }
     }
    },
    {
     "name": "change-bar/border",
     "type": "COLOR",
     "scopes": [
      "STROKE_COLOR"
     ],
     "web": "var(--change-bar-border)",
     "values": {
      "Dark": {
       "hex": "ffffff",
       "alpha": 0.1
      },
      "Light": {
       "hex": "1f3340",
       "alpha": 0.11
      }
     }
    },
    {
     "name": "subagent-tag/fill",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "var(--subagent-tag-fill)",
     "values": {
      "Dark": {
       "hex": "94beff",
       "alpha": 0.09
      },
      "Light": {
       "hex": "1b2a35",
       "alpha": 0.055
      }
     }
    },
    {
     "name": "subagent-tag/border",
     "type": "COLOR",
     "scopes": [
      "STROKE_COLOR"
     ],
     "web": "var(--subagent-tag-border)",
     "values": {
      "Dark": {
       "hex": "94beff",
       "alpha": 0.24
      },
      "Light": {
       "hex": "1f3340",
       "alpha": 0.14
      }
     }
    },
    {
     "name": "workbar/fill",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "var(--workbar-fill)",
     "values": {
      "Dark": {
       "hex": "0e131d",
       "alpha": 0.74
      },
      "Light": {
       "hex": "eef3f8",
       "alpha": 0.78
      }
     }
    },
    {
     "name": "workbar/border",
     "type": "COLOR",
     "scopes": [
      "STROKE_COLOR"
     ],
     "web": "var(--workbar-border)",
     "values": {
      "Dark": {
       "hex": "ffffff",
       "alpha": 0.09
      },
      "Light": {
       "hex": "1f3340",
       "alpha": 0.1
      }
     }
    },
    {
     "name": "workbar/track",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "var(--workbar-track)",
     "values": {
      "Dark": {
       "hex": "ffffff",
       "alpha": 0.13
      },
      "Light": {
       "hex": "1b2a35",
       "alpha": 0.11
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
       "hex": "141a25",
       "alpha": 0.42
      },
      "Light": {
       "hex": "ffffff",
       "alpha": 0.48
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
       "hex": "141a25",
       "alpha": 0.64
      },
      "Light": {
       "hex": "ffffff",
       "alpha": 0.72
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
       "hex": "141a25",
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
       "alpha": 0.1
      },
      "Light": {
       "hex": "182838",
       "alpha": 0.11
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
       "alpha": 0.16
      },
      "Light": {
       "hex": "ffffff",
       "alpha": 0.88
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
       "hex": "176fd0",
       "alpha": 1
      },
      "Light": {
       "hex": "176fd0",
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
       "hex": "0b4cc4",
       "alpha": 1
      },
      "Light": {
       "hex": "0b4cc4",
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
       "hex": "0d1422",
       "alpha": 1
      },
      "Light": {
       "hex": "e2ebf7",
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
       "hex": "090e19",
       "alpha": 1
      },
      "Light": {
       "hex": "e9f0f9",
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
       "hex": "060910",
       "alpha": 1
      },
      "Light": {
       "hex": "eff4fb",
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
       "hex": "05070b",
       "alpha": 1
      },
      "Light": {
       "hex": "f4f7fc",
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
       "hex": "3b82f6",
       "alpha": 0.18
      },
      "Light": {
       "hex": "ffffff",
       "alpha": 0.9
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
       "alpha": 0.06
      },
      "Light": {
       "hex": "ffffff",
       "alpha": 0.32
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
       "hex": "94beff",
       "alpha": 0.12
      },
      "Light": {
       "hex": "1552c6",
       "alpha": 0.12
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
       "hex": "94beff",
       "alpha": 0.35
      },
      "Light": {
       "hex": "1552c6",
       "alpha": 0.35
      }
     }
    },
    {
     "name": "tint/live-fill",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "var(--tint-live-fill)",
     "values": {
      "Dark": {
       "hex": "7fdfc8",
       "alpha": 0.1
      },
      "Light": {
       "hex": "146857",
       "alpha": 0.1
      }
     }
    },
    {
     "name": "tint/live-border",
     "type": "COLOR",
     "scopes": [
      "STROKE_COLOR"
     ],
     "web": "var(--tint-live-border)",
     "values": {
      "Dark": {
       "hex": "7fdfc8",
       "alpha": 0.42
      },
      "Light": {
       "hex": "146857",
       "alpha": 0.42
      }
     }
    },
    {
     "name": "tint/attention-fill",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "var(--tint-attention-fill)",
     "values": {
      "Dark": {
       "hex": "f0be82",
       "alpha": 0.1
      },
      "Light": {
       "hex": "7b4b06",
       "alpha": 0.1
      }
     }
    },
    {
     "name": "tint/attention-border",
     "type": "COLOR",
     "scopes": [
      "STROKE_COLOR"
     ],
     "web": "var(--tint-attention-border)",
     "values": {
      "Dark": {
       "hex": "f0be82",
       "alpha": 0.42
      },
      "Light": {
       "hex": "7b4b06",
       "alpha": 0.42
      }
     }
    },
    {
     "name": "tint/danger-fill",
     "type": "COLOR",
     "scopes": [
      "FRAME_FILL",
      "SHAPE_FILL"
     ],
     "web": "var(--tint-danger-fill)",
     "values": {
      "Dark": {
       "hex": "f4a8a0",
       "alpha": 0.1
      },
      "Light": {
       "hex": "a42531",
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
       "hex": "f4a8a0",
       "alpha": 0.45
      },
      "Light": {
       "hex": "a42531",
       "alpha": 0.45
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
       "hex": "edf1f7",
       "alpha": 0.08
      },
      "Light": {
       "hex": "162230",
       "alpha": 0.08
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
       "hex": "05070b",
       "alpha": 0.45
      },
      "Light": {
       "hex": "f4f7fc",
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
     "name": "spacing/prose",
     "type": "FLOAT",
     "scopes": [
      "GAP"
     ],
     "web": "var(--spacing-prose)",
     "values": {
      "Value": 14
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
     "name": "size/session-row",
     "type": "FLOAT",
     "scopes": [
      "WIDTH_HEIGHT"
     ],
     "web": "var(--size-session-row)",
     "values": {
      "Value": 44
     }
    },
    {
     "name": "size/turn-line",
     "type": "FLOAT",
     "scopes": [
      "WIDTH_HEIGHT"
     ],
     "web": "var(--size-turn-line)",
     "values": {
      "Value": 28
     }
    },
    {
     "name": "size/change-bar",
     "type": "FLOAT",
     "scopes": [
      "WIDTH_HEIGHT"
     ],
     "web": "var(--size-change-bar)",
     "values": {
      "Value": 36
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
      "Value": 268
     }
    },
    {
     "name": "size/reading",
     "type": "FLOAT",
     "scopes": [
      "WIDTH_HEIGHT"
     ],
     "web": "var(--size-reading)",
     "values": {
      "Value": 680
     }
    },
    {
     "name": "size/composer",
     "type": "FLOAT",
     "scopes": [
      "WIDTH_HEIGHT"
     ],
     "web": "var(--size-composer)",
     "values": {
      "Value": 720
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
      "Value": 320
     }
    },
    {
     "name": "size/panel-wide",
     "type": "FLOAT",
     "scopes": [
      "WIDTH_HEIGHT"
     ],
     "web": "var(--size-panel-wide)",
     "values": {
      "Value": 560
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
const DRY = typeof APPLY_FOUNDATIONS === "undefined" || APPLY_FOUNDATIONS !== true;
const page = figma.root.children.find(node => node.id === '229:3');
if (!page) throw Error('Inspect the existing Foundations page before synchronization');
await figma.setCurrentPageAsync(page);
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
report.collectionIds = (await figma.variables.getLocalVariableCollectionsAsync()).map(value => value.id);
report.variableIds = (await figma.variables.getLocalVariablesAsync()).map(value => value.id);
report.textStyleIds = (await figma.getLocalTextStylesAsync()).map(value => value.id);
report.pageId = page.id;
return report;
