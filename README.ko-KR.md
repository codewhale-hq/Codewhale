<!-- source: README.md sha256:75bb6a506af5 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="brand/wordmark-inverted.svg">
  <img src="brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**어떤 모델과도 함께 쓰는 오픈 소스 코딩 에이전트.**

Codewhale은 프로젝트를 읽고, 파일을 편집하고, 명령을 실행하며, 자신의 작업을
스스로 확인합니다. 터미널에서, 사용자가 고른 호스팅 모델 또는 로컬 모델로
동작합니다.

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[웹사이트](https://codewhale.net) · [문서](docs/README.md) · [변경 내역](CHANGELOG.md) · [기여하기](CONTRIBUTING.md)

[English](README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="web/public/codewhale-tui-0102-demo.png" alt="Codewhale 터미널 세션" width="760">

<sub>설정된 로컬 데모 세션에서 캡처한 실제 Codewhale 터미널 화면입니다.</sub>

</div>

## 설치

macOS와 Linux:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

설치 스크립트는 체크섬을 검증한 바이너리를 `~/.local/bin`에 내려받습니다.
이후 `codewhale`이 "command not found"라고 나오면 설치 스크립트가 출력하는
PATH 설정 한 줄을 실행하거나
[PATH에 추가하기](docs/INSTALL.md#put-it-on-your-path)를 참고하세요.
업그레이드는 언제든 `codewhale update`로 할 수 있습니다.

<details>
<summary><b>Windows, npm, Cargo 등 다른 설치 방법</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # build from crates.io
```

Docker, Nix, Linux의 Homebrew, Android/Termux, 체크섬 검증을 포함한 수동
다운로드, 선택 사항인 CNB 미러는 [설치 안내서](docs/INSTALL.md)에서 다룹니다.
방법은 하나만 고르세요. 한 컴퓨터에 여러 방식으로 설치하면 `PATH`를 두고
서로 충돌합니다.

</details>

## 빠른 시작

**공급자 API 키를 한곳에서 관리하세요.** [Codewhale 계정을 만들거나](https://app.codewhale.net/register)
`codewhale login`을 실행해 로그인하세요. 계정에서 API 키를 저장하고 업데이트한 뒤,
로그인한 기기에서 Codewhale 모델 경로를 통해 사용할 수 있습니다.
[계정 키 설정하기](docs/CONFIGURATION.md#account-provider-keys).
로그인해도 기존 로컬 키는 업로드되지 않으며, 계정 없이도 로컬에서 사용할 수 있습니다.

1. **프로젝트를 엽니다.** 작업할 폴더에서 `codewhale`을 실행합니다.
2. **모델을 연결합니다.** `/provider`를 실행하거나 `F3`을 눌러 호스팅 모델
   키를 추가하거나 로컬 런타임을 고릅니다. Ollama가 채팅 모델과 함께 이미
   실행 중이면 Codewhale이 알아서 그쪽으로 전환합니다. 모델을 바꾸려면
   `/model`을 사용하세요.
3. **구체적인 작업을 맡깁니다.**

```text
Fix the failing tests and explain what changed.
```

같은 작업을 스크립트나 CI 작업에서 헤드리스로 실행할 수도 있습니다.

```bash
codewhale exec --auto "fix the failing tests and explain what changed"
```

명령과 단축키는 `/help`에서 확인하세요.

## 실행 방법

모든 클라이언트는 같은 로컬 Codewhale Runtime을 사용하므로, 세션, 도구, 권한이
어디서나 똑같이 동작합니다.

| 명령 | 하는 일 |
| --- | --- |
| `codewhale` | 대화형 터미널 인터페이스 |
| `codewhale exec "…"` | 스크립트나 CI에서 헤드리스로 한 턴 실행. `--auto`는 파일 및 셸 도구를 허용하고, `--output-format stream-json`은 JSON을 스트리밍 |
| `codewhale web` | `127.0.0.1`에서 제공되는 내장 [로컬 브라우저 클라이언트](docs/WEB.md) |
| `codewhale review --pr N` | 참고용 [풀 리퀘스트 리뷰](docs/GITHUB_ACTION.md). 게시는 직접 선택해야 합니다 |
| Runtime API | 스레드, 이벤트, 승인을 다루는 [로컬 HTTP API](docs/RUNTIME_API.md) |

네이티브 데스크톱 앱(GPUI)은 로그인형 제품 클라이언트로 개발 중입니다. 제공
여부는 [제품 페이지](https://codewhale.net/en/product)에서 확인하세요.
커뮤니티가 관리하는
[VS Code 확장](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)은
사이드바에서 같은 Runtime에 연결합니다
([소스](https://github.com/HengQuWorld/CodeWhale-VSCode)).

## 할 수 있는 일

- **어떤 모델이든, 종속 없이.** Anthropic, DeepSeek, Google, Mistral,
  Moonshot, OpenAI, OpenRouter, xAI 등 40개가 넘는 내장 제공업체 경로에,
  OpenAI 호환 엔드포인트와 Ollama, vLLM, SGLang을 통한 로컬 모델까지
  지원합니다. [제공업체](docs/PROVIDERS.md)
- **통제권은 사용자에게.** Plan 모드는 아무것도 바꾸지 않고 살펴보기만 하고,
  Work와 Operate는 변경을 수행합니다. 승인 방식(posture)이 도구 호출에 언제
  사용자의 확인이 필요한지 정하고, `/undo`와 `/restore`로 작업 공간의 변경을
  되돌리며, `/receipts`는 세션의 모든 파일, 명령, 승인을 보여 줍니다.
  [모드](docs/MODES.md) · [영수증](docs/RECEIPTS.md)
- **긴 작업을 위해.** 지속되는 `/goal`을 세우고, 범위가 정해진 작업은
  [하위 에이전트](docs/SUBAGENTS.md)에 맡기고, 사전 비용 확인을 거치는 감독형
  [에이전트 팀](docs/FLEET.md)을 실행하거나, 저장소에 포함된
  [워크플로](docs/WORKFLOW_AUTHORING.md)로 스크립트화할 수 있습니다.
- **이미 쓰는 도구를 확장.** [MCP 서버](docs/MCP.md)를 연결하고,
  [스킬](docs/SKILLS.md)과 [플러그인](docs/PLUGINS.md)을 설치하고, 세션과 도구
  이벤트에서 [훅](docs/HOOKS.md)을 실행하고, 기존
  [Claude Code 플러그인](docs/CLAUDE_PLUGIN_COMPAT.md)도 불러올 수 있습니다.
- **Computer Use.** 기본 포함된 플러그인이 다른 애플리케이션을 관찰하고
  조작하는 도구를 추가합니다. 사용 전에 접근 범위를 검토하고 활성화하세요.
  [안내서](crates/tui/plugins/computer-use/README.md)

## 모드와 권한

| | 선택 방법 | 옵션 |
| --- | --- | --- |
| **모드** — 에이전트가 하는 일 | `Tab` 또는 `/mode` | Plan(탐색, 변경 없음) · Work(편집 및 실행) · Operate(계획되고 검증된 단계로 목표를 끝까지 수행) |
| **승인 방식(posture)** — 먼저 묻는 시점 | `Shift+Tab` | Ask · Auto-Review · Full Access |

Full Access에서도 강제 정책의 경계는 지켜집니다.
[모드 및 권한 안내서](docs/MODES.md)에서 각 옵션을 설명합니다.

## 안전

Codewhale은 사용자의 컴퓨터에서 사용자가 부여한 권한으로 실행됩니다. 승인
방식과 저장소 규칙이 에이전트가 할 수 있는 일을 제한하며, 지원되는 환경에서는
명령이 OS 샌드박스 안에서 실행됩니다(macOS는 Seatbelt, Linux의 bubblewrap은
설치되어 정상 작동하는 경우). `/preview-request`는 무언가 전송되기 전에 민감 정보를 가린 실제
요청 내용을 그대로 보여 줍니다. 가격을 알 수 없는 모델은 무료로 표시하지 않고
알 수 없음으로 둡니다.

[권한 확인 순서](docs/AUTHORIZATION_ORDER.md), [샌드박스](docs/SANDBOX.md),
[텔레메트리](docs/TELEMETRY.md)를 참고하세요. 사용량 집계는 기본으로 켜져 있으며
`codewhale config set telemetry false`로 끌 수 있습니다.

## 문서

| 시작하기 | 더 깊이 알아보기 |
| --- | --- |
| [설치](docs/INSTALL.md) | [설정](docs/CONFIGURATION.md) |
| [제공업체와 로컬 모델](docs/PROVIDERS.md) | [아키텍처](docs/ARCHITECTURE.md) |
| [모드와 권한](docs/MODES.md) | [Runtime API](docs/RUNTIME_API.md) |
| [단축키](docs/KEYBINDINGS.md) | [플러그인 작성](docs/PLUGIN_AUTHORING.md) |
| [GitHub PR 리뷰](docs/GITHUB_ACTION.md) | [전체 문서](docs/README.md) |

## 커뮤니티

버그 신고, 기능 제안, 풀 리퀘스트를 환영합니다. Codewhale을 몇 달째 쓰고 있든
처음 써 보든 상관없습니다. 빠진 제공업체가 있거나 불편한 작업 흐름이 있다면
[이슈를 열거나](https://github.com/codewhale-hq/CodeWhale/issues/new/choose)
[풀 리퀘스트를 보내 주세요](CONTRIBUTING.md). 첫 기여도 환영하며, 반영된 작업의
기여 이력은 기여자에게 남습니다.
[저장소 구조](CONTRIBUTING.md#project-structure)에서 시작하면 좋습니다.

[Discord](https://discord.gg/37gfS3ksug)에 참여하거나, WeChat에서 Hunter
(`hunterbown`)를 추가하고 Whale Brothers 그룹 가입을 요청하세요.

## 역사와 라이선스

Codewhale은 `deepseek-tui`에서 시작했으며 지금도 그 프로젝트의 설정과 세션을
읽습니다. 현재는 특정 제공업체에 얽매이지 않고 독립적으로 유지되며, 어떤 모델
제공업체와도 제휴 관계가 없습니다. 성장에 도움을 준
[모든 기여자](docs/CONTRIBUTORS.md)와 오픈 소스 커뮤니티에 감사드립니다.

[MIT](LICENSE). 다른 오픈 소스 프로젝트에서 가져와 수정한 부분은
[서드파티 고지](docs/THIRD_PARTY_NOTICES.md)에 기록되어 있습니다.
