<!-- source: README.md sha256:7e0e7ebc680b -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/wordmark-inverted.svg">
  <img src="../../brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**आपके टर्मिनल के लिए Rust में लिखा ओपन-सोर्स कोडिंग एजेंट। कोई भी मॉडल, मल्टी-एजेंट फ्लीट, OS सैंडबॉक्सिंग और सत्यापन योग्य रसीदें।**

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[वेबसाइट](https://codewhale.net) · [दस्तावेज़](../README.md) · [बदलाव सूची](../../CHANGELOG.md) · [योगदान](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="../../web/public/codewhale-tui-latest.png" width="760" alt="लोकल Ollama (qwen3:4b) पर एक असली कार्य के बाद टर्मिनल में Codewhale v0.10.4: इसने ls.py को संपादित किया, python3 -m unittest -v test_ls.py चलाने से पहले अनुमति का इंतज़ार किया, 2 पास होने वाले टेस्ट चलाए और परिणाम बताया।">

<sub>Accept Edits मोड में लोकल Ollama (qwen3:4b) चलाते हुए प्रकाशित Codewhale v0.10.4 (48669ad108fa) पर वास्तविक सत्र।</sub>

</div>

## इंस्टॉल करें

ऊपर दी गई इंस्टॉल स्क्रिप्ट macOS और Linux पर चेकसम-सत्यापित `codewhale` और इसके छोटे उपनाम `codew` को `~/.local/bin` में रखती है। यदि आपका शेल `command not found` बताता है, तो `~/.local/bin` को अपने `PATH` में जोड़ें ([विवरण](../INSTALL.md#put-it-on-your-path))। सीधे अपग्रेड करने के लिए `codewhale update` चलाएँ।

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

Linux, macOS और Windows (x64 और arm64) के लिए पूर्व-निर्मित बाइनरी उपलब्ध हैं। Scoop, Nix, Linux पर Homebrew, Android/Termux (प्रीव्यू) और मैनुअल डाउनलोड [docs/INSTALL.md](../INSTALL.md) में हैं; कंटेनर इमेज [docs/DOCKER.md](../DOCKER.md) में हैं। प्रति मशीन एक ही इंस्टॉल तरीका इस्तेमाल करें ताकि `PATH` पर दो बाइनरी एक-दूसरे को ओवरशैडो न करें।

## त्वरित शुरुआत

1. अपने काम को commit या stash करें, फिर किसी git रिपॉजिटरी के रूट से `codewhale` चलाएँ।
2. प्रदाता चुनने, API कुंजी पेस्ट करने और मॉडल चुनने के लिए `/provider` चलाएँ (या `F3` दबाएँ)। DeepSeek डिफ़ॉल्ट है; यदि कोई कुंजी कॉन्फ़िगर नहीं है और लोकल Ollama सर्वर जवाब देता है, तो Codewhale आपके पहले संदेश पर उसके मॉडल सुझाता है।
3. इसे कोई कार्य दें:

```text
Fix the failing tests and explain what changed.
```

केवल-पढ़ने वाले कमांड तुरंत चलते हैं। बदलाव करने वाले शेल कमांड अनुमति मांगते हैं: `y` एक बार चलाता है, `a` शेष सत्र के लिए मिलते-जुलते कमांड की अनुमति देता है, और `n` मना करता है। पिछले टर्न और उसके फ़ाइल बदलावों को वापस लेने के लिए `/undo` चलाएँ, या कमांड और कीबाइंडिंग के लिए `/help` देखें।

## क्षमताएँ

- **50+ बिल्ट-इन प्रदाता और लोकल मॉडल:** Anthropic, OpenAI, DeepSeek (डिफ़ॉल्ट रूप से `deepseek-flash` / V4.1 Flash; [विवरण](../PROVIDERS.md#deepseek-v41-flash)), Google, Mistral, Moonshot, OpenRouter, xAI, कोई भी OpenAI-संगत एंडपॉइंट, और Ollama, vLLM या SGLang के ज़रिए लोकल रनटाइम। सत्र के बीच `/model` से मॉडल बदलें, `/model auto` (`/router`) से प्रति-टर्न रूटिंग करें, और `/turn inspect` से लेटेंसी व लागत देखें। देखें [प्रदाता](../PROVIDERS.md)।
- **रसीदें और रोलबैक:** `/receipts` (या बाहर निकलने के बाद `codewhale receipts --last`) बदली गई फ़ाइलें, कमांड, वेब/MCP कॉल, सब-एजेंट, अनुमतियाँ और विफलताएँ दर्ज करता है। `/undo` एक चरण में पिछले टर्न को वापस लेता है; `/restore` वर्कस्पेस को पिछले स्नैपशॉट पर लौटाता है (`/trust on` या Full Access आवश्यक)। देखें [रसीदें](../RECEIPTS.md)।
- **सब-एजेंट, फ्लीट और वर्कफ़्लो:** `/goal` से बहु-टर्न लक्ष्य चलाएँ, काम को [सब-एजेंटों](../SUBAGENTS.md) को सौंपें, भूमिका- और मॉडल-विशिष्ट [फ्लीट (Fleets)](../FLEET.md) का समन्वय करें, या रिपॉजिटरी में सहेजे [वर्कफ़्लो](../WORKFLOW_AUTHORING.md) चलाएँ।
- **MCP, स्किल्स, हुक्स और प्लगइन:** [MCP सर्वर](../MCP.md), [स्किल्स](../SKILLS.md), [प्लगइन](../PLUGINS.md) और लाइफसाइकिल [हुक्स](../HOOKS.md) जोड़ें। प्रयोगात्मक कोड मोड (`execute_tools`) आपके सामान्य अनुमति नियमों के तहत कई MCP/प्लगइन कॉल को एक साथ चलाता है। वैकल्पिक macOS [Computer Use](https://github.com/codewhale-hq/codewhale-cu-plugin) तब तक बंद रहता है जब तक आप स्पष्ट रूप से इसकी समीक्षा कर इसे सक्षम न करें।
- **सीधी संगतता:** `AGENTS.md` को स्वचालित रूप से पढ़ता है। [config.example.toml](../../config.example.toml) में `project_instruction_imports` के ज़रिए `CLAUDE.md`, `.cursorrules` या `.github/copilot-instructions.md` चुनें। `/import-claude` से Claude Code सेटिंग्स और MCP सर्वर आयात करें, या `/plugin install` से Claude Code प्लगइन (स्किल्स, कमांड, एजेंट प्रोफ़ाइल, MCP) और DeepSeek Harness (DSH) प्लगइन इंस्टॉल करें ([विवरण](../CLAUDE_PLUGIN_COMPAT.md))।

## एंट्री पॉइंट

| कमांड | उद्देश्य |
| --- | --- |
| `codewhale` | इंटरैक्टिव टर्मिनल ऐप |
| `codewhale exec "…"` | स्क्रिप्ट और CI के लिए हेडलेस सिंगल-टास्क रनर ([गाइड](../INSTALL.md#headless-scripts-ci); `--auto` सैंडबॉक्स न होने पर Full Access उपयोग करता है) |
| `codewhale web` | `127.0.0.1` पर लोकल वेब क्लाइंट ([गाइड](../WEB.md)) |
| `codewhale review --pr N` | सलाहकारी GitHub PR समीक्षा; [GitHub बॉट सेटअप](../GITHUB_ACTION.md#set-up-your-own-bot) आपकी अपनी कुंजी पर चलता है |
| `codewhale app-server --http` | थ्रेड, इवेंट, अनुमतियों और प्रति-टर्न डिफ़ के लिए लोकल HTTP [Runtime API](../RUNTIME_API.md) |
| `codewhale serve --acp` | Zed जैसे एडिटर के लिए stdio पर Agent Client Protocol |

सभी एंट्री पॉइंट एक ही Rust इंजन साझा करते हैं ([आर्किटेक्चर](../ARCHITECTURE.md))। समुदाय द्वारा संचालित [VS Code एक्सटेंशन](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode) ([स्रोत](https://github.com/HengQuWorld/CodeWhale-VSCode)) उसी इंजन से जुड़ता है। वर्तमान रिलीज़ और डेस्कटॉप/क्लाउड स्थिति के लिए [नवीनतम रिलीज़ नोट्स](https://github.com/codewhale-hq/Codewhale/releases/latest) और [रोडमैप](../ROADMAP.md) देखें।

## अनुमतियाँ, सैंडबॉक्सिंग और गोपनीयता

`Shift+Tab` अनुमति मोड बदलता है: **Ask → Accept Edits → Plan → Auto-Review → Full Access** (`Tab` केवल ऑटोकम्प्लीट करता है)। **Accept Edits** (डिफ़ॉल्ट) git रिपॉजिटरी के भीतर संपादन लागू करता है और कमांड से पहले पूछता है। **Ask** हर संपादन और कमांड पर पूछता है। **Plan** केवल-पढ़ने के लिए है। **Auto-Review** नियमित कॉल को स्वीकृत करने और उच्च-जोखिम वाले कॉल को रोकने के लिए नियमों और समीक्षक मॉडल का उपयोग करता है। **Full Access** प्रॉम्प्ट छोड़ देता है और OS सैंडबॉक्स बंद कर देता है (रिपॉजिटरी नियम और प्रबंधित नीति फिर भी लागू रहती है)। देखें [मोड](../MODES.md)।

Ask, Accept Edits और Auto-Review में, जहाँ उपलब्ध हो वहाँ कमांड OS सैंडबॉक्स में चलते हैं: macOS पर Seatbelt और Linux पर bubblewrap (Windows पर अभी OS सैंडबॉक्स नहीं है)। सैंडबॉक्स किए गए कमांड और टर्मिनल सत्र `~/.ssh`, `~/.aws` या `.env` जैसी क्रेडेंशियल फ़ाइलें नहीं पढ़ सकते। भेजने से पहले संपादित (redacted) आउटगोइंग अनुरोध देखने के लिए `/preview-request` चलाएँ।

`/provider` से जोड़ी गई API कुंजियाँ आपकी मशीन पर ही रहती हैं, और लोकल मॉडल के लिए कुंजी की ज़रूरत नहीं होती। कीमत ज्ञात होने पर `/cost` और फ़ुटर सत्र का खर्च दिखाते हैं (अज्ञात कीमत को मुफ़्त नहीं, बल्कि अज्ञात दिखाया जाता है)।

एग्रीगेट टेलीमेट्री डिफ़ॉल्ट रूप से चालू है (संस्करण, OS, सत्र की अवधि/परिणाम, बिल्ट-इन प्रदाता ID, बिल्ट-इन सार्वजनिक कैटलॉग मॉडल ID (कस्टम, निजी या स्थानीय मॉडल `other` के रूप में गिने जाते हैं), कुल टोकन, बिल्ट-इन टूल कॉल/त्रुटि गणना और फ़ीचर/त्रुटि काउंटर; हर 90 दिन में बदलने वाली रैंडम इंस्टॉल ID; कभी भी कोड, प्रॉम्प्ट, बातचीत, कस्टम/निजी मॉडल नाम, फ़ाइल/रेपो/ब्रांच नाम, क्रेडेंशियल या IP पते नहीं)। इसे बंद करने के लिए चलाएँ:

```bash
codewhale config set telemetry false
```

स्टार्टअप पर Codewhale GitHub पर नए रिलीज़ की भी जाँच करता है (छोड़ने के लिए `CODEWHALE_NO_UPDATE_CHECK=1`) और हर 24 घंटे में सार्वजनिक `models.dev` मॉडल कैटलॉग को रीफ़्रेश करता है (छोड़ने के लिए `CODEWHALE_DISABLE_MODELS_DEV_FETCH=1` या `[catalog] auto_refresh = false`; CI और `DO_NOT_TRACK` में स्वतः बंद रहता है)। विवरण: [अधिकार क्रम](../AUTHORIZATION_ORDER.md), [सैंडबॉक्सिंग](../SANDBOX.md) और [टेलीमेट्री](../TELEMETRY.md)।

## दस्तावेज़

गाइड `main` शाखा का अनुसरण करते हैं और ऐसे बदलाव बता सकते हैं जो अभी नवीनतम रिलीज़ में नहीं आए हैं।

| यहाँ से शुरू करें | संदर्भ |
| --- | --- |
| [उपयोगकर्ता गाइड](../GUIDE.md) | [कॉन्फ़िगरेशन](../CONFIGURATION.md) |
| [इंस्टॉलेशन](../INSTALL.md) | [आर्किटेक्चर](../ARCHITECTURE.md) |
| [प्रदाता और लोकल मॉडल](../PROVIDERS.md) | [Runtime API](../RUNTIME_API.md) |
| [मोड और अनुमतियाँ](../MODES.md) | [प्लगइन निर्माण](../PLUGIN_AUTHORING.md) |
| [कीबाइंडिंग](../KEYBINDINGS.md) | [रसीदें](../RECEIPTS.md) |
| [GitHub PR समीक्षा](../GITHUB_ACTION.md) | [सभी दस्तावेज़](../README.md) |

## योगदान और लाइसेंस

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="Codewhale योगदानकर्ता वॉल"></a>

[योगदानकर्ता वॉल](../CONTRIBUTORS.md) (`scripts/contributor-wall.py`) कोड, सह-लिखित कमिट, आगे बढ़ाए गए PR और श्रेय प्राप्त रिपोर्ट को ट्रैक करती है। बग रिपोर्ट और फ़ीचर प्रस्ताव [Issues](https://github.com/codewhale-hq/Codewhale/issues/new/choose) में, प्रश्न [Discussions](https://github.com/codewhale-hq/Codewhale/discussions) में जाएँ, और PR दिशानिर्देश व लेआउट [CONTRIBUTING.md](../../CONTRIBUTING.md) ([प्रोजेक्ट संरचना](../../CONTRIBUTING.md#project-structure)) में हैं। [Discord](https://discord.gg/37gfS3ksug) पर जुड़ें या Whale Brothers समूह के लिए WeChat पर `hunterbown` जोड़ें।

Codewhale की शुरुआत `deepseek-tui` के रूप में हुई थी और यह इसके कॉन्फ़िग व सत्रों के साथ बैकवर्ड-संगत है। इसका रखरखाव स्वतंत्र रूप से किया जाता है और यह DeepSeek या किसी अन्य मॉडल प्रदाता से संबद्ध नहीं है।

[MIT](../../LICENSE)। तृतीय-पक्ष सूचनाएँ: [सम्मिलित स्रोत](../../THIRD_PARTY_NOTICES.md) और [अनुकूलित डिज़ाइन](../THIRD_PARTY_NOTICES.md).
