<!-- source: README.md sha256:0430d94f3d47 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/wordmark-inverted.svg">
  <img src="../../brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**어떤 모델과도 함께 쓸 수 있는 오픈 소스 터미널용 코딩 에이전트.**

Codewhale에 작업을 맡기면 저장소를 읽고, 파일을 편집하고, 명령을 실행하면서
작업이 끝나거나 사용자의 입력이 필요할 때까지 진행합니다. 40개가 넘는 내장
제공업체(기본값은 DeepSeek V4.1 Flash)와 사용자의 컴퓨터에서 실행되는 모델을 쓸 수
있습니다. `/receipts`는 Codewhale이 변경한 파일, 실행한 명령, 수행한 웹 및
MCP 호출을 보여 줍니다.

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[웹사이트](https://codewhale.net) · [문서](../README.md) · [변경 내역](../../CHANGELOG.md) · [기여하기](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="../../web/public/codewhale-tui-0102-demo.png" alt="Codewhale 터미널 세션" width="760">

<sub>스크립트로 동작하는 테스트 모델을 쓴 0.10.2 개발 빌드를 버려도 되는 작업 공간에서 전체 접근 권한으로 실행한 화면입니다. 편집, 테스트 세 개, 하위 에이전트 리뷰는 실제로 실행되었습니다.</sub>

</div>

<p align="center">
  <a href="https://www.orcarouter.ai/redeem/1a6250545fdc4e0aaccb0a4470159e67" rel="sponsored">
    <img src="../../web/public/codewhale-x-orcarouter.webp" alt="Codewhale × OrcaRouter — DeepSeek v4.1 Flash 사용량 30% 추가" width="760">
  </a>
</p>

[Codewhale × OrcaRouter 혜택 보기](https://www.orcarouter.ai/redeem/1a6250545fdc4e0aaccb0a4470159e67). Codewhale은 OrcaRouter에서 추천 수수료를 받습니다.

## 설치

macOS와 Linux:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

설치 스크립트는 체크섬을 검증한 `codewhale`과 짧은 별칭 `codew`를
`~/.local/bin`에 설치합니다. 이후 셸에서 `command not found`가 나오면 설치
스크립트가 출력하는 PATH 설정 줄을 실행하세요
([자세히](../INSTALL.md#put-it-on-your-path)). 업그레이드는
`codewhale update`로 합니다. 이 명령들은 최신 릴리스를 설치합니다. 이 페이지는
`main`을 따르므로, 버전이 표시된 기능은 그 릴리스에서 제공됩니다. 각 릴리스에
추가된 내용은 [변경 내역](../../CHANGELOG.md)에서 볼 수 있습니다.

<details>
<summary><b>Windows, npm, Cargo 등 다른 설치 방법</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # crates.io; can trail GitHub Releases
```

미리 빌드된 바이너리는 x64와 arm64의 Linux, macOS, Windows를 지원합니다. Nix,
Linux의 Homebrew, Android/Termux(프리뷰), 수동 다운로드는
[설치 안내서](../INSTALL.md)에서, 컨테이너는 [Docker](../DOCKER.md)에서
다룹니다. 한 컴퓨터에는 설치 방법을 하나만 쓰세요. 그렇지 않으면 `PATH`에
사본이 두 개 생기고, 먼저 오는 쪽이 실행됩니다.

</details>

## 빠른 시작

1. 작업 중인 변경을 커밋하거나 stash한 다음, git 저장소의 루트에서
   `codewhale`을 실행합니다. 기본 설정에서는 그 저장소의 파일 편집을 묻지 않고
   적용하고 diff를 보여 줍니다.
2. `/provider`를 실행하거나 `F3`을 눌러 제공업체를 고르고, API 키를 붙여 넣은
   뒤 모델을 선택합니다. 기본 제공업체는 DeepSeek입니다. 키가 없으면 대신 로컬
   런타임을 고르세요. 설정된 제공업체가 없고 Ollama가 이미 채팅 모델을 제공하고
   있으면 Codewhale이 자동으로 그 모델을 사용합니다. 0.10.2부터는 첫 메시지를
   보낼 때 그 모델을 쓰도록 제안합니다.
3. 작업을 입력합니다.

```text
Fix the failing tests and explain what changed.
```

읽기 전용 명령은 바로 실행됩니다. 그 밖의 셸 명령은 Codewhale이 먼저 묻습니다.
`y`는 한 번만 실행하고, `a`는 남은 대화 동안 같은 종류의 명령을 허용하며,
`n`은 거부합니다. `/undo`는 마지막 요청과 그 요청의 파일 변경을 되돌립니다. 명령과 단축키 목록은 `/help`에서
볼 수 있습니다.

스크립트와 CI용으로는 인터페이스 없이 작업 하나를 실행하는 `codewhale exec`
명령이 있습니다. 기본적으로 `--auto`는 전체 접근 권한을 사용합니다. 도구는
묻지 않고 실행되고, 셸 명령에는 Codewhale 샌드박스가 적용되지 않으며, 신뢰
모드에서는 파일 도구가 작업 공간 밖에도 쓸 수 있습니다.
`--sandbox workspace-write`를 추가하면 신뢰 모드를 끄고 셸 샌드박스와 파일
도구의 작업 공간 제한을 유지할 수 있습니다.

```bash
codewhale exec --auto --sandbox workspace-write "fix the failing tests and explain what changed"
```

## 모드와 권한

새로 설치하면 실행 모드와 확인 권한으로 시작합니다.

| | 전환 방법 | 옵션 |
| --- | --- | --- |
| 모드 | `Tab`(입력란이 비어 있을 때) 또는 `/mode` | **계획**(Plan)은 읽고 검색하지만 아무것도 바꾸지 않습니다. **실행**(Work)은 파일을 편집하고 명령을 실행합니다. **운영**(Operate)은 실행과 같은 접근 권한을 가지며, 요청을 목표로 다루고 목표가 검증되거나 사용자의 입력이 필요할 때까지 계속 진행합니다. |
| 권한 | `Shift+Tab` | **확인**(Ask)은 저장소 안의 파일 편집은 묻지 않고 적용하고, 읽기 전용이 아닌 셸 명령은 실행 전에 묻습니다. **자동 검토**(Auto-Review)는 규칙과 리뷰어 모델이 일상적인 호출을 승인하게 하고 위험도가 높은 호출은 거부합니다. 그런 호출을 직접 승인하려면 확인으로 전환하세요. **전체 접근**(Full Access)은 전혀 묻지 않으며 Codewhale의 샌드박스를 끕니다. 저장소 규칙과 관리 정책은 계속 적용됩니다. |

자세한 내용은 [모드와 권한 안내서](../MODES.md)를 참고하세요.

## 할 수 있는 일

### 0.10.2의 새로운 기능

- `/pet on`은 터미널을 에이전트가 하는 일을 보여 주는 움직이는 고래로 바꾸며,
  그동안에도 메시지 입력란은 계속 쓸 수 있습니다. 980개의 입자가 현재 단계를
  영어, 중국어 또는 일본어 글자로 그려 냅니다.
- `/workbar terminal`은 에이전트가 실행 중인 셸을 실행되는 그대로 보여 줍니다.
  그중 하나에 직접 입력하거나, `Ctrl+N`을 눌러 자신만의 셸을 열 수 있습니다
  (macOS와 Linux).
- `/undo`는 마지막 요청 전체를 그 요청이 변경한 모든 파일까지 포함해 한 번에
  되돌리고, `/diff`는 Git이 없는 폴더에서도 동작합니다.
- 계획이 준비되면, Codewhale은 실행 모드를 확인 권한으로
  시작할지 자동 검토 권한으로 시작할지, 아니면 계획을 계속할지 묻습니다.
- `/color`는 채팅마다 고유한 입력란 색을 지정하며, 이 색은 세션과 함께
  저장됩니다.
- `web_search`는 DeepSeek의 새 Search API를 사용할 수 있습니다. DeepSeek 키와
  함께 `[search] provider = "deepseek"`를 설정하세요.
- 전체 접근이 아닐 때는 자격 증명을 업로드하게 될 `curl`이나 `wget` 명령이
  사용자의 승인 없이 실행되는 일이 없으며, 보이지 않는 유니코드를 숨긴 명령은
  거부됩니다. `write`는 에이전트가 읽지 않은 파일이나 읽은 뒤에 변경된 파일을
  더 이상 덮어쓰지 않습니다.

### 모델

Anthropic, OpenAI, DeepSeek, Google, Mistral, Moonshot, OpenRouter, xAI를
포함해 40개가 넘는 제공업체가 내장되어 있습니다. OpenAI 호환 엔드포인트라면
어느 것이든 쓸 수 있고, Ollama, vLLM, SGLang을 통한 로컬 모델도 쓸 수
있습니다. 기본값은 DeepSeek의 `deepseek-flash`이며, DeepSeek는 2026-10-17에 이
모델을 DeepSeek-V4.1-Flash-1017로 바꿨습니다. 0.10.1은 업데이트 없이 이 모델을
실행합니다([자세히](../PROVIDERS.md#deepseek-v41-flash)).

`/model`로 턴 사이에 모델을 바꾸고, `/model auto`는 턴마다 모델을 고릅니다
(고르는 방식은 `/router`로 설정합니다). `/turn inspect`는 한 턴에 쓰인 모델,
비용, 걸린 시간을 보여 줍니다. `codewhale auth chatgpt`를 쓰면 API 키 대신
ChatGPT 요금제를 사용할 수 있습니다(실험적). [제공업체](../PROVIDERS.md)를
참고하세요.

### 영수증과 되돌리기

`/receipts`(종료한 뒤에는 `codewhale receipts --last`)는 변경된 파일, 실행된
명령, 웹 및 MCP 호출, 시작된 에이전트, 승인 내역과 승인한 주체, 실패를 보여
줍니다. `/undo`는 마지막 요청과 그 요청이 변경한 파일을 신뢰 모드 없이 한 번에 되돌립니다. `/restore`는 작업 공간 전체를 이전
스냅샷으로 되돌리는데, 이후의 편집을 덮어쓰기 때문에 `/trust on` 또는 전체
접근 권한이 필요합니다. [영수증](../RECEIPTS.md)을 참고하세요.

도구를 사용할 수 있는 exec 실행은 모든 출력 형식에서 저장되므로, `--json`을
포함한 `exec --auto` 실행 후에도 `codewhale receipts --last`를 쓸 수 있습니다.
답변만 하는 텍스트 및 `--json` 실행은 재개하거나 대화 압축이 시작될 때만
저장됩니다. 영수증이나 `exec --continue`를 위해 항상 저장하려면
`--output-format stream-json`을 사용하세요.

### 긴 작업

`/goal`은 여러 턴에 걸쳐 유지되는 목표를 정합니다. Codewhale은 작업을
[하위 에이전트](../SUBAGENTS.md)에 나눠 맡기거나, 여러 에이전트로 구성된
[Fleet](../FLEET.md)를 실행하거나, 저장소에 커밋된
[워크플로](../WORKFLOW_AUTHORING.md)를 따를 수 있습니다.

### 다른 에이전트에서 옮겨 오는 경우

Codewhale은 따로 설정하지 않아도 `AGENTS.md`를 읽습니다. `CLAUDE.md`,
`.cursorrules`, `.github/copilot-instructions.md`처럼 다른 도구용으로 작성된
지침 파일은 설정에서 `project_instruction_imports = ["claude"]`(또는
`"cursor"`, `"cline"`, `"windsurf"`, `"copilot"`, `"all"`)로 직접 켠 경우에만
불러옵니다. [config.example.toml](../../config.example.toml)을 참고하세요. 불러오지
않은 지침 파일을 발견하면 Codewhale이 경고를 표시합니다.

`/import-claude`는 사용자가 검토할 수 있도록 Claude Code 설정과 MCP 서버
가져오기를 준비합니다. `/plugin install`은 스킬, 명령, 에이전트 프로필, MCP
서버를 담은 Claude Code 플러그인을 받아들이고, 훅이나 LSP 서버가 필요한
플러그인은 거부합니다([자세히](../CLAUDE_PLUGIN_COMPAT.md)).

### 확장

[MCP 서버](../MCP.md)를 연결하고, [스킬](../SKILLS.md)과
[플러그인](../PLUGINS.md)을 추가하고, 세션 및 도구 이벤트에서
[훅](../HOOKS.md)을 실행할 수 있습니다. 코드 모드(실험적, 기본으로 켜짐)에서는
모델이 여러 MCP 및 플러그인 도구를 호출하는 `execute_tools` 프로그램 하나를
작성할 수 있으며, 각 호출은 여전히 승인 규칙을 따르고 영수증에 남습니다. 기본
별도로 설치하는 Computer Use
플러그인(실험적, macOS 전용)은 다른 앱을 관찰하고 조작할 수 있습니다. 접근
범위를 검토하고 활성화하기 전까지는 꺼져 있습니다
([안내서](https://github.com/codewhale-hq/codewhale-cu-plugin)).

## 실행 방법

| 명령 | 하는 일 |
| --- | --- |
| `codewhale` | 대화형 터미널 앱 |
| `codewhale exec "…"` | 인터페이스 없이 작업 하나를 실행합니다. 옵션 없이 쓰면 도구를 사용하지 않고 답변만 하고, `--auto`는 기본적으로 전체 접근 권한으로 도구를 활성화하고, `--sandbox workspace-write`는 도구를 제한하며, `--output-format stream-json`은 이벤트를 JSON으로 스트리밍합니다 |
| `codewhale web` | `127.0.0.1`에서 동작하는 로컬 웹 UI([안내서](../WEB.md)) |
| `codewhale review --pr N` | GitHub 풀 리퀘스트에 대한 참고용 리뷰. [GitHub 봇 설정](../GITHUB_ACTION.md#set-up-your-own-bot)은 자동 리뷰와 `@codewhale review`를 위한 버전이 고정된 워크플로를 생성하며, 직접 모델 키와 선택적인 App 신원을 사용합니다 |
| `codewhale app-server --http` | 스레드, 이벤트, 승인, 각 턴의 파일 변경을 다루는 로컬 HTTP API인 [Runtime API](../RUNTIME_API.md) |
| `codewhale serve --acp` | Zed 같은 에디터를 위한 stdio 기반 Agent Client Protocol |

터미널 앱, `exec`, `web`, Runtime API는 하나의 엔진을 공유합니다
([아키텍처](../ARCHITECTURE.md)). 커뮤니티가 관리하는
[VS Code 확장](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)은
사이드바에서 이 엔진에 연결됩니다([소스](https://github.com/HengQuWorld/CodeWhale-VSCode)).
Runtime API를 기반으로 한 네이티브 데스크톱 앱이 개발 중이며, 아직 공개
다운로드는 없습니다.

## 비용과 계정

Codewhale은 무료이며 MIT 라이선스로 배포됩니다. 자신의 API 키를 쓰면
제공업체에 직접 비용을 내고, 로컬 모델은 키가 필요 없습니다. 가격을 알 수
있으면 하단 표시줄과 `/cost`에 세션 비용이 표시되며, 가격을 알 수 없으면 무료가
아니라 알 수 없음으로 표시합니다.

계정은 없어도 됩니다. `/provider`로 추가한 키는 사용자의 컴퓨터에 보관됩니다.

## 안전과 개인정보

확인과 자동 검토 권한에서는 시스템에 OS 샌드박스가 있으면 그 안에서 명령이
실행됩니다. macOS에서는 Seatbelt를, Linux에서는 bubblewrap이 설치되어 있으면
bubblewrap을 사용합니다. Windows에는 아직 OS 샌드박스가 없으며, 전체 접근은
샌드박스를 끕니다. 샌드박스 안에서 셸 명령은 `~/.ssh`, `~/.aws`, `.env` 파일
같은 자격 증명 저장소를 읽을 수 없으며, 0.10.2부터는 같은 규칙이 터미널 세션과
더 많은 파일 도구에도 적용됩니다. `/preview-request`는 다음 요청을 보내지 않고, 민감 정보를
가린 미리 보기를 보여 줍니다.

텔레메트리는 기본으로 켜져 있습니다. 90일마다 바뀌는 무작위 설치 ID와 함께
집계된 수치(버전과 플랫폼, 세션 길이와 결과, 사용한 내장 제공업체, 기능 및
오류 횟수)를 보내며, 대화, 코드, 프롬프트, 모델 이름, 파일 또는 저장소 이름,
자격 증명은 절대 보내지 않습니다.
`codewhale config set telemetry false`로 끌 수 있습니다. Codewhale은 시작할 때
GitHub에서 새 릴리스도 확인합니다. 이 확인을 건너뛰려면
`CODEWHALE_NO_UPDATE_CHECK=1`을 설정하세요. 자세한 내용은
[권한 확인 순서](../AUTHORIZATION_ORDER.md), [샌드박스](../SANDBOX.md),
[텔레메트리](../TELEMETRY.md)를 참고하세요.

## 문서

| 시작하기 | 참고 자료 |
| --- | --- |
| [설치](../INSTALL.md) | [설정](../CONFIGURATION.md) |
| [제공업체와 로컬 모델](../PROVIDERS.md) | [아키텍처](../ARCHITECTURE.md) |
| [모드와 권한](../MODES.md) | [Runtime API](../RUNTIME_API.md) |
| [단축키](../KEYBINDINGS.md) | [플러그인 작성](../PLUGIN_AUTHORING.md) |
| [GitHub PR 리뷰](../GITHUB_ACTION.md) | [전체 문서](../README.md) |

## 커뮤니티

빠진 제공업체가 있거나 불편한 작업 흐름이 있으면
[이슈를 열거나](https://github.com/codewhale-hq/CodeWhale/issues/new/choose)
[풀 리퀘스트를 보내 주세요](../../CONTRIBUTING.md). 반영된 작업의 기여 이력은
기여자에게 남으며, 무엇이 어디에 있는지는
[저장소 구조](../../CONTRIBUTING.md#project-structure)에서 볼 수 있습니다.

[Discord](https://discord.gg/37gfS3ksug)에서 이야기하거나, WeChat에서 메인테이너
Hunter(`hunterbown`)를 추가하고 Whale Brothers 그룹 가입을 요청하세요.

## 역사와 라이선스

Codewhale은 `deepseek-tui`에서 시작했으며 지금도 그 프로젝트의 설정과 세션을
읽습니다. 독립적으로 유지되며 DeepSeek를 비롯한 어떤 모델 제공업체와도 제휴
관계가 없습니다. [기여해 주신 모든 분](../CONTRIBUTORS.md)께 감사드립니다.

[![Codewhale contributors](../assets/contributors.png)](../CONTRIBUTORS.md#everyone)

[MIT](../../LICENSE). 다른 오픈 소스 프로젝트에서 가져와 수정한 코드는
[서드파티 고지](../THIRD_PARTY_NOTICES.md)에 정리되어 있습니다.
