<!-- source: README.md sha256:75bb6a506af5 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="brand/wordmark-inverted.svg">
  <img src="brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**Her modelle çalışan açık kaynaklı kodlama ajanı.**

Codewhale projenizi okur, dosyaları düzenler, komutları çalıştırır ve kendi işini
kontrol eder — terminalinizde, seçtiğiniz barındırılan veya yerel bir modelle.

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Web sitesi](https://codewhale.net) · [Belgeler](docs/README.md) · [Değişiklik günlüğü](CHANGELOG.md) · [Katkıda bulunma](CONTRIBUTING.md)

[English](README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="web/public/codewhale-tui-0102-demo.png" alt="Bir Codewhale terminal oturumu" width="760">

<sub>Yapılandırılmış yerel bir demo oturumundan gerçek Codewhale terminal görüntüsü.</sub>

</div>

## Kurulum

macOS ve Linux:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

Kurulum betiği, sağlama toplamı doğrulanmış ikili dosyaları `~/.local/bin` dizinine
indirir. Ardından `codewhale` "command not found" derse, betiğin yazdırdığı tek satırlık
PATH komutunu çalıştırın veya [PATH'e ekleme](docs/INSTALL.md#put-it-on-your-path)
bölümüne bakın. İstediğiniz zaman `codewhale update` ile yükseltebilirsiniz.

<details>
<summary><b>Windows, npm, Cargo ve diğer yollar</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # build from crates.io
```

Docker, Nix, Linux'ta Homebrew, Android/Termux, sağlama toplamı doğrulamalı elle
indirmeler ve isteğe bağlı CNB yansısı [kurulum kılavuzunda](docs/INSTALL.md) anlatılır.
Tek bir yol seçin: aynı makinedeki birden fazla kurulum `PATH` üzerinde birbiriyle çakışır.

</details>

## Hızlı başlangıç

**Sağlayıcı API anahtarlarınızı tek bir yerden yönetin.** [Codewhale hesabı oluşturun](https://app.codewhale.net/register)
veya oturum açmak için `codewhale login` komutunu çalıştırın. API anahtarlarınızı hesabınıza kaydedip güncelleyin,
ardından oturum açtığınız cihazlarda Codewhale model rotası üzerinden kullanın.
[Hesap anahtarlarını ayarlayın](docs/CONFIGURATION.md#account-provider-keys).
Oturum açmak mevcut yerel anahtarları yüklemez; hesap olmadan da yerel kullanım mümkündür.

1. **Projenizi açın.** Üzerinde çalışmak istediğiniz klasörde `codewhale` çalıştırın.
2. **Bir model bağlayın.** Barındırılan bir anahtar eklemek veya yerel bir çalışma ortamı
   seçmek için `/provider` komutunu çalıştırın (veya `F3` tuşuna basın). Ollama zaten bir
   sohbet modeliyle çalışıyorsa Codewhale kendiliğinden ona geçer. Modeli değiştirmek için
   `/model` kullanın.
3. **Ona somut bir görev verin.**

```text
Fix the failing tests and explain what changed.
```

Aynı görev bir betikten veya CI işinden arayüzsüz de çalışır:

```bash
codewhale exec --auto "fix the failing tests and explain what changed"
```

Komutlar ve klavye kısayolları için `/help` çalıştırın.

## Çalıştırma yolları

Her istemci aynı yerel Codewhale Runtime'ı kullanır; bu nedenle oturumlar, araçlar ve
izinler her yerde aynı şekilde davranır.

| Komut | Ne yapar |
| --- | --- |
| `codewhale` | Etkileşimli terminal arayüzü |
| `codewhale exec "…"` | Bir betikten veya CI'dan tek bir arayüzsüz tur; `--auto` dosya ve kabuk araçlarına izin verir, `--output-format stream-json` JSON akışı sağlar |
| `codewhale web` | `127.0.0.1` üzerinde paketle gelen [yerel tarayıcı istemcisi](docs/WEB.md) |
| `codewhale review --pr N` | Tavsiye niteliğinde bir [pull request incelemesi](docs/GITHUB_ACTION.md); yayınlamak isteğe bağlıdır |
| Runtime API | İş parçacıkları, olaylar ve onaylar için [yerel bir HTTP API](docs/RUNTIME_API.md) |

Oturum açılan ürün istemcisi olarak yerel bir masaüstü uygulaması (GPUI) geliştiriliyor;
kullanılabilirlik için [ürün sayfasına](https://codewhale.net/en/product) bakın. Topluluk
tarafından sürdürülen [VS Code eklentisi](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)
kenar çubuğundan aynı Runtime'a bağlanır ([kaynak](https://github.com/HengQuWorld/CodeWhale-VSCode)).

## Ne yapar

- **Her model, kilitlenme yok.** 40'tan fazla yerleşik sağlayıcı yolu — Anthropic,
  DeepSeek, Google, Mistral, Moonshot, OpenAI, OpenRouter, xAI ve daha fazlası — ayrıca
  OpenAI uyumlu her uç nokta ve Ollama, vLLM veya SGLang üzerinden yerel modeller.
  [Sağlayıcılar](docs/PROVIDERS.md)
- **Kontrol sizde kalır.** Plan modu hiçbir şeyi değiştirmeden keşfeder; Work ve Operate
  değişiklik yapar. Onay duruşları hangi araç çağrısının onayınıza ihtiyaç duyduğunu
  belirler, `/undo` ve `/restore` çalışma alanı değişikliklerini geri getirir, `/receipts`
  ise bir oturumdaki her dosyayı, komutu ve onayı listeler. [Modlar](docs/MODES.md) ·
  [Makbuzlar](docs/RECEIPTS.md)
- **Uzun işler için yapıldı.** Kalıcı bir `/goal` belirleyin, sınırlı işleri
  [alt ajanlara](docs/SUBAGENTS.md) devredin, harcamadan önce kontrol yapan gözetimli
  [ajan ekipleri](docs/FLEET.md) çalıştırın veya bunları depoya eklenmiş
  [iş akışları](docs/WORKFLOW_AUTHORING.md) olarak betikleyin.
- **Zaten kullandıklarınızı genişletin.** [MCP sunucuları](docs/MCP.md) bağlayın,
  [beceriler](docs/SKILLS.md) ve [eklentiler](docs/PLUGINS.md) kurun, oturum ve araç
  olaylarında [kancalar](docs/HOOKS.md) çalıştırın ve mevcut
  [Claude Code eklentilerini](docs/CLAUDE_PLUGIN_COMPAT.md) yükleyin.
- **Computer Use.** Dahil edilen bir eklenti, diğer uygulamaları gözlemlemek ve
  kullanmak için araçlar ekler. Kullanmadan önce erişimini gözden geçirin ve etkinleştirin.
  [Kılavuz](crates/tui/plugins/computer-use/README.md)

## Modlar ve izinler

| | Nasıl seçilir | Seçenekler |
| --- | --- | --- |
| **Mode** — ajanın ne yaptığı | `Tab` veya `/mode` | Plan (keşfet, değişiklik yok) · Work (düzenle ve çalıştır) · Operate (bir hedefi planlı, doğrulanmış adımlarla yürüt) |
| **Posture** — ne zaman önce sorduğu | `Shift+Tab` | Ask · Auto-Review · Full Access |

Full Access de katı politika sınırlarına uyar.
[Modlar ve izinler kılavuzu](docs/MODES.md) her seçeneği açıklar.

## Güvenlik

Codewhale makinenizde, ona verdiğiniz erişimle çalışır. Onay duruşları ve depo kuralları
ajanın yapabileceklerini sınırlar; komutlar, desteklenen yerlerde bir işletim sistemi
korumalı alanı içinde çalışır (macOS'ta Seatbelt; Linux'ta bubblewrap, kurulu ve çalışıyorsa).
`/preview-request`, herhangi bir şey gönderilmeden önce tam ve gizlenmiş (redacted)
isteği gösterir. Fiyatı bilinmeyen modeller ücretsiz olarak gösterilmez, bilinmeyen olarak kalır.

[Yetkilendirme sırası](docs/AUTHORIZATION_ORDER.md), [korumalı alan](docs/SANDBOX.md) ve
[telemetri](docs/TELEMETRY.md) bölümlerine bakın — kullanım sayıları varsayılan olarak
açıktır ve `codewhale config set telemetry false` bunları kapatır.

## Belgeler

| Buradan başlayın | Daha derine inin |
| --- | --- |
| [Kurulum](docs/INSTALL.md) | [Yapılandırma](docs/CONFIGURATION.md) |
| [Sağlayıcılar ve yerel modeller](docs/PROVIDERS.md) | [Mimari](docs/ARCHITECTURE.md) |
| [Modlar ve izinler](docs/MODES.md) | [Runtime API](docs/RUNTIME_API.md) |
| [Tuş atamaları](docs/KEYBINDINGS.md) | [Eklenti geliştirme](docs/PLUGIN_AUTHORING.md) |
| [GitHub PR incelemesi](docs/GITHUB_ACTION.md) | [Tüm belgeler](docs/README.md) |

## Topluluk

Hata raporları, özellik fikirleri ve pull request'ler memnuniyetle karşılanır — ister
Codewhale'i aylardır kullanıyor olun, ister ilk kez deneyin. Bir sağlayıcı eksikse veya
bir iş akışı zahmetliyse [bir issue açın](https://github.com/codewhale-hq/CodeWhale/issues/new/choose)
ya da [pull request gönderin](CONTRIBUTING.md). İlk katkılar memnuniyetle karşılanır ve
katkıda bulunanlar, kabul edilen işin hakkını korur. [Depo yapısı](CONTRIBUTING.md#project-structure)
başlamak için iyi bir yerdir.

[Discord](https://discord.gg/37gfS3ksug) sunucusuna katılın veya WeChat'te Hunter'ı
(`hunterbown`) ekleyip Whale Brothers grubuna katılmak istediğinizi söyleyin.

## Geçmiş ve lisans

Codewhale, `deepseek-tui` olarak başladı ve hâlâ o projenin yapılandırmasını ve
oturumlarını okuyor. Artık sağlayıcıdan bağımsız, bağımsız olarak sürdürülüyor ve hiçbir
model sağlayıcısıyla bağlantılı değil. [Tüm katkıda bulunanlara](docs/CONTRIBUTORS.md) ve
büyümesine yardım eden açık kaynak topluluklarına teşekkürler.

[MIT](LICENSE). Diğer açık kaynak projelerden uyarlanan bölümler
[üçüncü taraf bildirimlerinde](docs/THIRD_PARTY_NOTICES.md) kayıtlıdır.
