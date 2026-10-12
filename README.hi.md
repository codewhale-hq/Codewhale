<!-- source: README.md sha256:75bb6a506af5 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="brand/wordmark-inverted.svg">
  <img src="brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**ओपन सोर्स कोडिंग एजेंट, जो किसी भी मॉडल के साथ काम करता है।**

Codewhale आपका प्रोजेक्ट पढ़ता है, फ़ाइलें संपादित करता है, कमांड चलाता है और अपने काम की
खुद जाँच करता है — आपके टर्मिनल में, आपके चुने हुए होस्ट किए गए या लोकल मॉडल के साथ।

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[वेबसाइट](https://codewhale.net) · [दस्तावेज़](docs/README.md) · [बदलावों की सूची](CHANGELOG.md) · [योगदान](CONTRIBUTING.md)

[English](README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="web/public/codewhale-tui-0102-demo.png" alt="टर्मिनल में चलता Codewhale सेशन" width="760">

<sub>कॉन्फ़िगर किए गए स्थानीय डेमो सत्र से Codewhale के असली टर्मिनल का कैप्चर।</sub>

</div>

## इंस्टॉल करें

macOS और Linux:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

इंस्टॉलर चेकसम से सत्यापित बाइनरी `~/.local/bin` में डाउनलोड करता है। अगर इसके बाद
`codewhale` चलाने पर "command not found" दिखे, तो इंस्टॉलर जो एक PATH लाइन प्रिंट
करता है उसे चलाएँ, या [इसे PATH में जोड़ें](docs/INSTALL.md#put-it-on-your-path) देखें।
कभी भी `codewhale update` से अपग्रेड करें।

<details>
<summary><b>Windows, npm, Cargo और अन्य तरीके</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # build from crates.io
```

Docker, Linux पर Nix और Homebrew, Android/Termux, चेकसम सत्यापन के साथ मैन्युअल
डाउनलोड और वैकल्पिक CNB मिरर [इंस्टॉलेशन गाइड](docs/INSTALL.md) में दिए गए हैं।
कोई एक तरीका चुनें: एक मशीन पर कई इंस्टॉलेशन `PATH` को लेकर आपस में टकराते हैं।

</details>

## क्विकस्टार्ट

**अपने प्रदाताओं की API कुंजियाँ एक ही जगह सँभालें।** [Codewhale खाता बनाएँ](https://app.codewhale.net/register)
या साइन इन करने के लिए `codewhale login` चलाएँ। अपने खाते में API कुंजियाँ सहेजें और अपडेट करें,
फिर साइन इन किए हुए उपकरणों पर Codewhale के मॉडल रूट के ज़रिए उनका इस्तेमाल करें।
[खाते की कुंजियाँ सेट अप करें](docs/CONFIGURATION.md#account-provider-keys)।
साइन इन करने से मौजूदा स्थानीय कुंजियाँ अपलोड नहीं होतीं; बिना खाते के भी स्थानीय रूप से काम कर सकते हैं।

1. **अपना प्रोजेक्ट खोलें।** जिस फ़ोल्डर में काम करना है, उसमें `codewhale` चलाएँ।
2. **कोई मॉडल जोड़ें।** होस्टेड कुंजी जोड़ने या कोई लोकल रनटाइम चुनने के लिए `/provider`
   चलाएँ (या `F3` दबाएँ)। अगर Ollama पहले से किसी चैट मॉडल के साथ चल रहा है, तो
   Codewhale अपने-आप उस पर चला जाता है। मॉडल बदलने के लिए `/model` इस्तेमाल करें।
3. **कोई ठोस काम बताएँ।**

```text
Fix the failing tests and explain what changed.
```

यही काम किसी स्क्रिप्ट या CI जॉब से बिना इंटरफ़ेस के भी चलता है:

```bash
codewhale exec --auto "fix the failing tests and explain what changed"
```

कमांड और कीबोर्ड शॉर्टकट देखने के लिए `/help` चलाएँ।

## इसे चलाने के तरीके

हर क्लाइंट उसी लोकल Codewhale Runtime को चलाता है, इसलिए सेशन, टूल और अनुमतियाँ हर जगह
एक जैसे व्यवहार करते हैं।

| कमांड | क्या करता है |
| --- | --- |
| `codewhale` | इंटरैक्टिव टर्मिनल इंटरफ़ेस |
| `codewhale exec "…"` | किसी स्क्रिप्ट या CI से एक बिना इंटरफ़ेस वाला टर्न; `--auto` फ़ाइल और शेल टूल की अनुमति देता है, `--output-format stream-json` JSON स्ट्रीम करता है |
| `codewhale web` | `127.0.0.1` पर पैकेज में शामिल [लोकल ब्राउज़र क्लाइंट](docs/WEB.md) |
| `codewhale review --pr N` | सलाह के रूप में [pull request समीक्षा](docs/GITHUB_ACTION.md); पोस्ट करना वैकल्पिक है |
| Runtime API | थ्रेड, इवेंट और अनुमोदन के लिए [लोकल HTTP API](docs/RUNTIME_API.md) |

एक नेटिव डेस्कटॉप ऐप (GPUI) साइन-इन किए गए प्रोडक्ट क्लाइंट के रूप में बनाया जा रहा है;
उपलब्धता के लिए [प्रोडक्ट पेज](https://codewhale.net/en/product) देखें। समुदाय द्वारा
अनुरक्षित [VS Code एक्सटेंशन](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)
साइडबार से उसी Runtime से जुड़ता है ([सोर्स](https://github.com/HengQuWorld/CodeWhale-VSCode))।

## यह क्या करता है

- **कोई भी मॉडल, कोई बंधन नहीं।** 40 से अधिक बिल्ट-इन प्रोवाइडर रूट — Anthropic,
  DeepSeek, Google, Mistral, Moonshot, OpenAI, OpenRouter, xAI और अन्य — साथ ही कोई भी
  OpenAI-संगत एंडपॉइंट और Ollama, vLLM या SGLang के ज़रिए लोकल मॉडल।
  [प्रोवाइडर](docs/PROVIDERS.md)
- **नियंत्रण आपके पास रहता है।** Plan मोड कुछ बदले बिना पड़ताल करता है; Work और Operate
  बदलाव करते हैं। अनुमोदन की स्थितियाँ तय करती हैं कि किस टूल कॉल के लिए आपकी मंज़ूरी
  चाहिए, `/undo` और `/restore` वर्कस्पेस के बदलाव वापस लाते हैं, और `/receipts` सेशन की
  हर फ़ाइल, कमांड और मंज़ूरी की सूची देता है। [मोड](docs/MODES.md) ·
  [रसीदें](docs/RECEIPTS.md)
- **लंबे कामों के लिए बना।** स्थायी `/goal` तय करें, सीमित काम
  [सब-एजेंटों](docs/SUBAGENTS.md) को सौंपें, खर्च से पहले जाँच के साथ निगरानी में चलने वाली
  [एजेंट टीमें](docs/FLEET.md) चलाएँ, या उन्हें रिपॉज़िटरी में रखे
  [वर्कफ़्लो](docs/WORKFLOW_AUTHORING.md) के रूप में स्क्रिप्ट करें।
- **जो आप पहले से इस्तेमाल करते हैं, उसे बढ़ाएँ।** [MCP सर्वर](docs/MCP.md) जोड़ें,
  [स्किल](docs/SKILLS.md) और [प्लगइन](docs/PLUGINS.md) इंस्टॉल करें, सेशन और टूल
  इवेंट पर [हुक](docs/HOOKS.md) चलाएँ, और मौजूदा
  [Claude Code प्लगइन](docs/CLAUDE_PLUGIN_COMPAT.md) लोड करें।
- **Computer Use।** शामिल प्लगइन दूसरे ऐप देखने और चलाने के टूल जोड़ता है। इस्तेमाल से
  पहले उसके एक्सेस की समीक्षा करें और उसे सक्षम करें।
  [गाइड](crates/tui/plugins/computer-use/README.md)

## मोड और अनुमतियाँ

| | कैसे चुनें | विकल्प |
| --- | --- | --- |
| **Mode** — एजेंट क्या कर रहा है | `Tab` या `/mode` | Plan (पड़ताल, कोई बदलाव नहीं) · Work (संपादन और कमांड चलाना) · Operate (नियोजित और सत्यापित चरणों से किसी लक्ष्य तक पहुँचना) |
| **Posture** — पहले कब पूछता है | `Shift+Tab` | Ask · Auto-Review · Full Access |

Full Access भी नीति की बाध्यकारी सीमाओं का पालन करता है।
[मोड और अनुमतियों की गाइड](docs/MODES.md) हर विकल्प को समझाती है।

## सुरक्षा

Codewhale आपकी मशीन पर उतने ही एक्सेस के साथ चलता है जितना आप उसे देते हैं। अनुमोदन की
स्थितियाँ और रिपॉज़िटरी के नियम एजेंट की गतिविधियों को सीमित करते हैं, और जहाँ समर्थन है
वहाँ कमांड OS सैंडबॉक्स के भीतर चलते हैं (macOS पर Seatbelt; Linux पर bubblewrap, जब वह इंस्टॉल हो और ठीक से काम कर रहा हो)।
कुछ भी भेजे जाने से पहले `/preview-request` सटीक, संपादित (redacted) अनुरोध दिखाता है।
जिन मॉडलों की कीमत ज्ञात नहीं है, उन्हें मुफ़्त बताने के बजाय अज्ञात ही दिखाया जाता है।

[अधिकार क्रम](docs/AUTHORIZATION_ORDER.md), [सैंडबॉक्सिंग](docs/SANDBOX.md) और
[टेलीमेट्री](docs/TELEMETRY.md) देखें — उपयोग की गिनती डिफ़ॉल्ट रूप से चालू रहती है और
`codewhale config set telemetry false` उसे बंद कर देता है।

## दस्तावेज़

| यहाँ से शुरू करें | और गहराई में जाएँ |
| --- | --- |
| [इंस्टॉलेशन](docs/INSTALL.md) | [कॉन्फ़िगरेशन](docs/CONFIGURATION.md) |
| [प्रोवाइडर और लोकल मॉडल](docs/PROVIDERS.md) | [आर्किटेक्चर](docs/ARCHITECTURE.md) |
| [मोड और अनुमतियाँ](docs/MODES.md) | [Runtime API](docs/RUNTIME_API.md) |
| [कीबाइंडिंग](docs/KEYBINDINGS.md) | [प्लगइन लेखन](docs/PLUGIN_AUTHORING.md) |
| [GitHub PR समीक्षा](docs/GITHUB_ACTION.md) | [सभी दस्तावेज़](docs/README.md) |

## समुदाय

बग रिपोर्ट, फ़ीचर के सुझाव और pull request का स्वागत है — चाहे आप Codewhale का कई महीनों से
इस्तेमाल कर रहे हों या पहली बार आज़मा रहे हों। अगर कोई प्रोवाइडर उपलब्ध नहीं है या कोई
वर्कफ़्लो असहज है, तो [issue खोलें](https://github.com/codewhale-hq/CodeWhale/issues/new/choose)
या [pull request भेजें](CONTRIBUTING.md)। पहले योगदान का स्वागत है, और स्वीकार किए गए काम का
श्रेय योगदानकर्ताओं के पास रहता है। [रिपॉज़िटरी की संरचना](CONTRIBUTING.md#project-structure)
शुरुआत के लिए अच्छी जगह है।

[Discord](https://discord.gg/37gfS3ksug) से जुड़ें, या WeChat पर Hunter (`hunterbown`) को जोड़कर
Whale Brothers समूह में शामिल होने के लिए कहें।

## इतिहास और लाइसेंस

Codewhale की शुरुआत `deepseek-tui` के रूप में हुई थी और यह आज भी उस प्रोजेक्ट का कॉन्फ़िगरेशन
और सेशन पढ़ता है। अब यह किसी प्रोवाइडर पर निर्भर नहीं है, स्वतंत्र रूप से अनुरक्षित है और किसी भी
मॉडल प्रोवाइडर से संबद्ध नहीं है। [हर योगदानकर्ता](docs/CONTRIBUTORS.md) और प्रोजेक्ट को आगे
बढ़ाने वाले ओपन सोर्स समुदायों का धन्यवाद।

[MIT](LICENSE)। अन्य ओपन सोर्स प्रोजेक्ट से लिए और अनुकूलित किए गए हिस्से
[थर्ड-पार्टी नोटिस](docs/THIRD_PARTY_NOTICES.md) में दर्ज हैं।
