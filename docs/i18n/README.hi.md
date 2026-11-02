<!-- source: README.md sha256:0430d94f3d47 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/wordmark-inverted.svg">
  <img src="../../brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**आपके टर्मिनल के लिए एक ओपन सोर्स कोडिंग एजेंट, जो किसी भी मॉडल के साथ काम करता है।**

Codewhale को कोई काम दें, और वह रिपॉज़िटरी पढ़ता है, फ़ाइलें संपादित करता है और कमांड
चलाता है, जब तक काम पूरा न हो जाए या उसे आपसे इनपुट की ज़रूरत न पड़े। यह 40 से अधिक
बिल्ट-इन प्रोवाइडर के साथ काम करता है (डिफ़ॉल्ट रूप से DeepSeek V4.1 Flash), और आपकी
अपनी मशीन पर चल रहे मॉडलों के साथ भी। `/receipts` उन फ़ाइलों की सूची देता है जो इसने बदलीं, उन
कमांड की जो इसने चलाईं, और उन वेब और MCP कॉल की जो इसने कीं।

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[वेबसाइट](https://codewhale.net) · [दस्तावेज़](../README.md) · [बदलावों की सूची](../../CHANGELOG.md) · [योगदान](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="../../web/public/codewhale-tui-0102-demo.png" alt="Codewhale का एक टर्मिनल सेशन" width="760">

<sub>0.10.2 का एक डेवलपमेंट बिल्ड, स्क्रिप्टेड टेस्ट मॉडल के साथ, एक अस्थायी वर्कस्पेस के भीतर Full Access में। संपादन, तीन टेस्ट और एक सब-एजेंट समीक्षा सचमुच चलीं।</sub>

</div>

<p align="center">
  <a href="https://www.orcarouter.ai/redeem/1a6250545fdc4e0aaccb0a4470159e67" rel="sponsored">
    <img src="../../web/public/codewhale-x-orcarouter.webp" alt="Codewhale × OrcaRouter — DeepSeek v4.1 Flash का 30% अतिरिक्त उपयोग" width="760">
  </a>
</p>

[Codewhale × OrcaRouter का ऑफ़र देखें](https://www.orcarouter.ai/redeem/1a6250545fdc4e0aaccb0a4470159e67). Codewhale को OrcaRouter से रेफ़रल शुल्क मिलता है।

## इंस्टॉल करें

macOS और Linux:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

इंस्टॉलर चेकसम से सत्यापित `codewhale` और उसका छोटा उपनाम `codew` `~/.local/bin` में
रखता है। अगर इसके बाद आपका शेल `command not found` कहे, तो इंस्टॉलर जो PATH लाइन
प्रिंट करता है, उसे चलाएँ ([विवरण](../INSTALL.md#put-it-on-your-path))।
`codewhale update` इसे अपग्रेड करता है। ये कमांड नवीनतम रिलीज़ इंस्टॉल करती हैं।
यह पेज `main` के अनुसार चलता है, इसलिए जिस फ़ीचर पर कोई संस्करण लिखा है, वह उसी रिलीज़
में आता है; [बदलावों की सूची](../../CHANGELOG.md) बताती है कि हर रिलीज़ में क्या जुड़ा।

<details>
<summary><b>Windows, npm, Cargo और इंस्टॉल करने के अन्य तरीके</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # crates.io; can trail GitHub Releases
```

पहले से बनी बाइनरी x64 और arm64 पर Linux, macOS और Windows के लिए उपलब्ध हैं। Nix,
Linux पर Homebrew, Android/Termux (प्रीव्यू) और मैन्युअल डाउनलोड
[इंस्टॉलेशन गाइड](../INSTALL.md) में हैं, और कंटेनर [Docker](../DOCKER.md) में।
हर मशीन पर इंस्टॉल का एक ही तरीका अपनाएँ; वरना `PATH` पर दो कॉपी हो जाती हैं और जो
पहले आती है, वही चलती है।

</details>

## क्विकस्टार्ट

1. अपना काम commit या stash करें, फिर किसी git रिपॉज़िटरी के रूट से `codewhale` चलाएँ।
   डिफ़ॉल्ट रूप से यह वहाँ फ़ाइल संपादन बिना पूछे लागू करता है और आपको diff दिखाता है।
2. `/provider` चलाएँ (या `F3` दबाएँ), कोई प्रोवाइडर चुनें, उसकी API कुंजी पेस्ट करें और
   एक मॉडल चुनें। DeepSeek डिफ़ॉल्ट प्रोवाइडर है। अगर कुंजी नहीं है, तो इसके बजाय कोई
   लोकल रनटाइम चुनें। अगर कोई प्रोवाइडर कॉन्फ़िगर नहीं है और Ollama पहले से कोई चैट
   मॉडल चला रहा है, तो Codewhale अपने-आप उसका इस्तेमाल करता है; 0.10.2 से, जब आप
   अपना पहला संदेश भेजते हैं, तब यह उसे इस्तेमाल करने का विकल्प देता है।
3. कोई काम लिखें:

```text
Fix the failing tests and explain what changed.
```

केवल पढ़ने वाली (read-only) कमांड तुरंत चलती हैं। किसी भी दूसरी शेल कमांड के लिए
Codewhale पहले पूछता है: `y` उसे एक बार चलाता है, `a` बाकी बातचीत के लिए उस जैसी
कमांड की अनुमति देता है, `n` मना कर देता है। `/undo` आपका पिछला अनुरोध और उससे हुए
फ़ाइल बदलाव वापस करता है। `/help` कमांड और
कीबोर्ड कुंजियों की सूची देता है।

स्क्रिप्ट और CI के लिए `codewhale exec` बिना इंटरफ़ेस के एक काम चलाता है। डिफ़ॉल्ट
रूप से `--auto` Full Access का इस्तेमाल करता है: टूल बिना पूछे चलते हैं, शेल कमांड
पर Codewhale का सैंडबॉक्स लागू नहीं होता, और ट्रस्ट मोड फ़ाइल टूल को वर्कस्पेस के
बाहर लिखने देता है। शेल सैंडबॉक्स और फ़ाइल टूल की वर्कस्पेस सीमाएँ बनाए रखने और
ट्रस्ट मोड बंद रखने के लिए `--sandbox workspace-write` जोड़ें:

```bash
codewhale exec --auto --sandbox workspace-write "fix the failing tests and explain what changed"
```

## मोड और अनुमतियाँ

नया इंस्टॉलेशन Work मोड और Ask अनुमति के साथ शुरू होता है।

| | कैसे बदलें | विकल्प |
| --- | --- | --- |
| मोड | `Tab` (जब इनपुट खाली हो) या `/mode` | **Plan** पढ़ता और खोजता है, पर कुछ नहीं बदलता। **Work** फ़ाइलें संपादित करता है और कमांड चलाता है। **Operate** के पास Work जितना ही एक्सेस है; यह आपके अनुरोध को एक लक्ष्य मानता है और तब तक चलता रहता है जब तक लक्ष्य सत्यापित न हो जाए या उसे आपसे इनपुट की ज़रूरत न पड़े। |
| अनुमति | `Shift+Tab` | **Ask** आपकी रिपॉज़िटरी में फ़ाइल संपादन बिना पूछे लागू करता है और जो शेल कमांड read-only नहीं हैं, उनसे पहले पूछता है। **Auto-Review** नियमों और एक समीक्षक मॉडल को सामान्य कॉल मंज़ूर करने देता है और उच्च जोखिम वाली कॉल मना कर देता है; उन्हें खुद मंज़ूर करने के लिए Ask पर जाएँ। **Full Access** कभी नहीं पूछता और Codewhale का सैंडबॉक्स बंद कर देता है; रिपॉज़िटरी के नियम और प्रबंधित नीति (managed policy) फिर भी लागू रहते हैं। |

विवरण के लिए [मोड और अनुमतियों की गाइड](../MODES.md) देखें।

## यह क्या करता है

### 0.10.2 में नया

- `/pet on` आपके टर्मिनल को एक एनिमेटेड व्हेल में बदल देता है, जो दिखाती है कि एजेंट
  क्या कर रहा है, और इस दौरान संदेश बॉक्स चालू रहता है। इसके 980 कण मौजूदा कदम को
  अंग्रेज़ी, चीनी या जापानी में लिखते हैं।
- `/workbar terminal` एजेंट द्वारा चलाए जा रहे शेल को चलते-चलते दिखाता है। उनमें से
  किसी में टाइप करें, या अपना खुद का शेल खोलने के लिए `Ctrl+N` दबाएँ (macOS और Linux)।
- `/undo` आपका पूरा पिछला अनुरोध एक ही कदम में वापस करता है, उससे बदली गई हर फ़ाइल
  समेत, और `/diff` बिना Git वाले फ़ोल्डरों में भी काम करता है।
- जब कोई प्लान तैयार होता है, तो Codewhale पूछता है कि Work को Ask या Auto-Review
  के साथ शुरू करें, या प्लानिंग जारी रखें।
- `/color` हर चैट के कंपोज़र को उसका अपना रंग देता है, जो सेशन के साथ सहेजा जाता है।
- `web_search` DeepSeek के नए Search API का इस्तेमाल कर सकता है: DeepSeek कुंजी के
  साथ `[search] provider = "deepseek"` सेट करें।
- Full Access के बाहर, कोई `curl` या `wget` जो कोई क्रेडेंशियल अपलोड करता, आपकी
  मंज़ूरी के बिना कभी नहीं चलता, और अदृश्य Unicode छिपाने वाली कमांड मना कर दी जाती
  है। `write` अब ऐसी फ़ाइल को नहीं बदलता जिसे एजेंट ने पढ़ा नहीं है, या जो उसके पढ़ने
  के बाद बदल गई है।

### मॉडल

40 से अधिक प्रोवाइडर बिल्ट-इन हैं, जिनमें Anthropic, OpenAI, DeepSeek, Google,
Mistral, Moonshot, OpenRouter और xAI शामिल हैं। कोई भी OpenAI-संगत एंडपॉइंट काम करता
है, और Ollama, vLLM या SGLang के ज़रिए लोकल मॉडल भी। डिफ़ॉल्ट मॉडल DeepSeek का
`deepseek-flash` है, जिसे DeepSeek ने 2026-10-17 को DeepSeek-V4.1-Flash-1017 पर
कर दिया; 0.10.1 इसे बिना अपडेट के चलाता है
([विवरण](../PROVIDERS.md#deepseek-v41-flash))।

`/model` टर्न के बीच मॉडल बदलता है, और `/model auto` हर टर्न के लिए एक मॉडल चुनता है
(`/router` तय करता है कि कैसे)। `/turn inspect` दिखाता है कि किसी टर्न ने कौन-सा मॉडल
इस्तेमाल किया, उस पर कितना खर्च हुआ और उसमें कितना समय लगा। `codewhale auth chatgpt`
से आप API कुंजी के बजाय ChatGPT प्लान इस्तेमाल कर सकते हैं (प्रायोगिक)।
[प्रोवाइडर](../PROVIDERS.md) देखें।

### रसीदें और पूर्ववत करना

`/receipts`, या बाहर निकलने के बाद `codewhale receipts --last`, बदली गई फ़ाइलों,
चलाई गई कमांड, वेब और MCP कॉल, शुरू किए गए एजेंट, अनुमोदन और उन्हें किसने दिया, और
विफलताओं की सूची देता है। `/undo` आपका पिछला अनुरोध और उससे बदली गई फ़ाइलें एक ही कदम में वापस करता है, बिना ट्रस्ट मोड के। `/restore` पूरे
वर्कस्पेस को किसी पिछले स्नैपशॉट पर वापस ले जाता है; क्योंकि इससे बाद के संपादन
ओवरराइट हो जाते हैं, इसके लिए `/trust on` या Full Access चाहिए।
[रसीदें](../RECEIPTS.md) देखें।

जिन exec रन में टूल उपलब्ध होते हैं, वे हर आउटपुट फ़ॉर्मैट में सहेजे जाते हैं, इसलिए
`codewhale receipts --last`, `exec --auto` के बाद काम करता है, `--json` के साथ भी।
केवल जवाब देने वाले टेक्स्ट और `--json` रन तभी सहेजे जाते हैं जब उन्हें फिर से शुरू
किया जाता है या बातचीत का कॉम्पैक्शन शुरू होता है; रसीदों या `exec --continue`
के लिए उन्हें हमेशा सहेजने के लिए `--output-format stream-json` इस्तेमाल करें।

### लंबे काम

`/goal` एक ऐसा उद्देश्य तय करता है जो कई टर्न तक बना रहता है। Codewhale काम को
[सब-एजेंटों](../SUBAGENTS.md) में बाँट सकता है, एजेंटों का एक [fleet](../FLEET.md)
चला सकता है, या रिपॉज़िटरी में रखे किसी [वर्कफ़्लो](../WORKFLOW_AUTHORING.md) का
पालन कर सकता है।

### किसी दूसरे एजेंट से आ रहे हैं

Codewhale `AGENTS.md` अपने-आप पढ़ता है। दूसरे टूल के लिए लिखी गई निर्देश फ़ाइलें, जैसे
`CLAUDE.md`, `.cursorrules` या `.github/copilot-instructions.md`, तभी लोड होती हैं जब
आप अपने कॉन्फ़िग में `project_instruction_imports = ["claude"]` (या `"cursor"`,
`"cline"`, `"windsurf"`, `"copilot"`, `"all"`) से इन्हें चालू करते हैं;
[config.example.toml](../../config.example.toml) देखें। जब Codewhale को ऐसी कोई फ़ाइल
मिलती है जिसे उसने लोड नहीं किया, तो वह चेतावनी देता है।

`/import-claude` आपकी Claude Code सेटिंग और MCP सर्वर का इंपोर्ट तैयार करता है, ताकि
आप उसकी समीक्षा कर सकें। `/plugin install` ऐसे Claude Code प्लगइन स्वीकार करता है
जिनमें स्किल, कमांड, एजेंट प्रोफ़ाइल और MCP सर्वर हों, और उन प्लगइन को मना कर देता है
जिन्हें हुक या LSP सर्वर चाहिए ([विवरण](../CLAUDE_PLUGIN_COMPAT.md))।

### एक्सटेंशन

[MCP सर्वर](../MCP.md) कनेक्ट करें, [स्किल](../SKILLS.md) और
[प्लगइन](../PLUGINS.md) जोड़ें, और सेशन व टूल इवेंट पर [हुक](../HOOKS.md) चलाएँ।
Code mode (प्रायोगिक, डिफ़ॉल्ट रूप से चालू) मॉडल को एक ऐसा `execute_tools` प्रोग्राम
लिखने देता है जो कई MCP और प्लगइन टूल कॉल करता है; हर कॉल फिर भी आपके अनुमोदन नियमों
से गुज़रती है और उसकी रसीद बनी रहती है। अलग से इंस्टॉल किया गया Computer Use प्लगइन (प्रायोगिक, केवल macOS) दूसरे ऐप देख और चला सकता
है। जब तक आप उसके एक्सेस की समीक्षा करके उसे सक्षम नहीं करते, वह बंद रहता है
([गाइड](https://github.com/codewhale-hq/codewhale-cu-plugin))।

## इसे चलाने के तरीके

| कमांड | क्या करता है |
| --- | --- |
| `codewhale` | इंटरैक्टिव टर्मिनल ऐप |
| `codewhale exec "…"` | बिना इंटरफ़ेस के एक काम। अकेले चलाने पर यह टूल का इस्तेमाल किए बिना जवाब देता है; `--auto` डिफ़ॉल्ट रूप से Full Access के साथ टूल चालू करता है, `--sandbox workspace-write` उन्हें सीमित करता है, और `--output-format stream-json` इवेंट को JSON के रूप में स्ट्रीम करता है |
| `codewhale web` | `127.0.0.1` पर एक लोकल वेब UI ([गाइड](../WEB.md)) |
| `codewhale review --pr N` | किसी GitHub pull request की सलाह के तौर पर समीक्षा। [GitHub बॉट सेटअप](../GITHUB_ACTION.md#set-up-your-own-bot) स्वचालित समीक्षाओं और `@codewhale review` के लिए एक पिन किया हुआ workflow बनाता है, जो आपकी अपनी मॉडल कुंजी और वैकल्पिक App पहचान का उपयोग करता है |
| `codewhale app-server --http` | [Runtime API](../RUNTIME_API.md), थ्रेड, इवेंट, अनुमोदन और हर टर्न के फ़ाइल बदलावों के लिए एक लोकल HTTP API |
| `codewhale serve --acp` | stdio पर Agent Client Protocol, Zed जैसे एडिटर के लिए |

टर्मिनल ऐप, `exec`, `web` और Runtime API एक ही इंजन साझा करते हैं
([आर्किटेक्चर](../ARCHITECTURE.md))। समुदाय द्वारा अनुरक्षित एक
[VS Code एक्सटेंशन](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)
साइडबार से इससे जुड़ता है ([सोर्स](https://github.com/HengQuWorld/CodeWhale-VSCode))।
Runtime API पर बने एक नेटिव डेस्कटॉप ऐप पर काम चल रहा है; उसका अभी कोई सार्वजनिक
डाउनलोड नहीं है।

## लागत और खाते

Codewhale मुफ़्त है और MIT लाइसेंस के तहत है। अपनी API कुंजी के साथ आप सीधे प्रोवाइडर
को भुगतान करते हैं; लोकल मॉडलों के लिए किसी कुंजी की ज़रूरत नहीं होती। जब कीमत ज्ञात
हो, तो फ़ुटर और `/cost` दिखाते हैं कि किसी सेशन पर कितना खर्च हुआ है, और अज्ञात कीमत
को अज्ञात ही दिखाया जाता है, कभी मुफ़्त नहीं।

आपको खाते की ज़रूरत नहीं है। `/provider` से जोड़ी गई कुंजियाँ आपकी मशीन पर ही रहती हैं।

## सुरक्षा और गोपनीयता

Ask और Auto-Review में, जहाँ सिस्टम में OS सैंडबॉक्स है, वहाँ कमांड उसी के भीतर चलती
हैं: macOS पर Seatbelt, और Linux पर bubblewrap, जब वह इंस्टॉल हो। Windows पर अभी कोई
OS सैंडबॉक्स नहीं है, और Full Access सैंडबॉक्स बंद कर देता है। सैंडबॉक्स के भीतर
शेल कमांड `~/.ssh`, `~/.aws` और `.env` फ़ाइलों जैसे क्रेडेंशियल स्टोर नहीं पढ़ सकतीं;
0.10.2 से यही नियम टर्मिनल सेशन और ज़्यादा फ़ाइल टूल पर भी लागू होता है।
`/preview-request` अगले अनुरोध को भेजे बिना उसका संपादित (redacted) प्रीव्यू दिखाता है।

टेलीमेट्री डिफ़ॉल्ट रूप से चालू है। यह समेकित गिनतियाँ (संस्करण और प्लेटफ़ॉर्म, सेशन
की अवधि और परिणाम, आपने कौन-से बिल्ट-इन प्रोवाइडर इस्तेमाल किए, फ़ीचर और त्रुटियों की
गिनती) एक रैंडम इंस्टॉल ID के साथ भेजती है, जो हर 90 दिन में बदल जाती है। यह कभी बातचीत,
कोड, प्रॉम्प्ट, मॉडल के नाम, फ़ाइल या रिपॉज़िटरी के नाम, या क्रेडेंशियल नहीं भेजती। `codewhale config set telemetry false` इसे बंद कर
देता है। शुरू होते समय Codewhale नई रिलीज़ के लिए GitHub भी जाँचता है; इसे छोड़ने के
लिए `CODEWHALE_NO_UPDATE_CHECK=1` सेट करें। ज़्यादा जानकारी
[अधिकार क्रम](../AUTHORIZATION_ORDER.md), [सैंडबॉक्सिंग](../SANDBOX.md) और
[टेलीमेट्री](../TELEMETRY.md) में है।

## दस्तावेज़

| यहाँ से शुरू करें | संदर्भ |
| --- | --- |
| [इंस्टॉलेशन](../INSTALL.md) | [कॉन्फ़िगरेशन](../CONFIGURATION.md) |
| [प्रोवाइडर और लोकल मॉडल](../PROVIDERS.md) | [आर्किटेक्चर](../ARCHITECTURE.md) |
| [मोड और अनुमतियाँ](../MODES.md) | [Runtime API](../RUNTIME_API.md) |
| [कीबाइंडिंग](../KEYBINDINGS.md) | [प्लगइन लेखन](../PLUGIN_AUTHORING.md) |
| [GitHub PR समीक्षा](../GITHUB_ACTION.md) | [सभी दस्तावेज़](../README.md) |

## समुदाय

अगर कोई प्रोवाइडर मौजूद नहीं है या कोई वर्कफ़्लो असहज है, तो
[issue खोलें](https://github.com/codewhale-hq/CodeWhale/issues/new/choose) या
[pull request भेजें](../../CONTRIBUTING.md)। मर्ज होने वाले काम का श्रेय योगदानकर्ताओं के
पास रहता है, और [रिपॉज़िटरी की संरचना](../../CONTRIBUTING.md#project-structure) बताती है कि
कौन-सी चीज़ कहाँ है।

[Discord](https://discord.gg/37gfS3ksug) पर बात करें, या WeChat पर मेंटेनर Hunter
(`hunterbown`) को जोड़ें और Whale Brothers समूह में शामिल होने के लिए कहें।

## इतिहास और लाइसेंस

Codewhale की शुरुआत `deepseek-tui` के रूप में हुई थी और यह आज भी उस प्रोजेक्ट का
कॉन्फ़िगरेशन और सेशन पढ़ता है। यह स्वतंत्र रूप से अनुरक्षित है और DeepSeek या किसी भी
अन्य मॉडल प्रोवाइडर से संबद्ध नहीं है।
[योगदान देने वाले हर व्यक्ति](../CONTRIBUTORS.md) का धन्यवाद।

[![Codewhale contributors](../assets/contributors.png)](../CONTRIBUTORS.md#everyone)

[MIT](../../LICENSE)। अन्य ओपन सोर्स प्रोजेक्ट से अनुकूलित कोड
[थर्ड-पार्टी नोटिस](../THIRD_PARTY_NOTICES.md) में सूचीबद्ध है।
