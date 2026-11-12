<!-- source: README.md sha256:fd76d718e3a2 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/mark-reversed.svg">
  <img src="../../brand/mark-mono.svg" alt="Codewhale" width="72" height="72">
</picture>

<h1>모델은 누구의 것이어도 됩니다. 에이전트는 당신의 것이어야 합니다.</h1>

Codewhale은 터미널에서 쓰는 오픈 소스 코딩 에이전트이며 Rust로 작성되었습니다. 어떤 모델과도 함께 작동하고, 하나의 작업을 여러 에이전트에게 나눌 수 있으며, 코드 한 줄 한 줄을 직접 읽을 수 있습니다.

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

이 명령은 macOS 또는 Linux에 최신 릴리스를 설치하며 계정이 필요 없습니다. [Windows, npm, Cargo](#설치)는 아래에 있습니다.

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[웹사이트](https://codewhale.net) · [문서](../README.md) · [변경 기록](../../CHANGELOG.md) · [기여하기](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<sub>이 번역은 기계 번역 초안이며 원어민 검토를 아직 받지 않았습니다.</sub>

<img src="../../web/public/codewhale-tui-0102-demo.png" width="760" alt="데모 작업을 마친 터미널의 Codewhale. export.py에 --json 플래그를 추가했고, 로컬 테스트 3개가 모두 통과했으며, 위임한 리뷰에서 문제가 없었다고 보고합니다. 아래 작업 목록에는 완료된 세 단계가 있고, 상태 줄에는 Full Access와 로컬 website-demo 제공자가 표시됩니다.">

<sub>스크립트로 만든 테스트 모델을 쓰는 0.10.3(아직 출시 전) 개발 빌드를 일회용 작업 공간에서 Full Access로 실행했습니다. 편집, 테스트 세 개, 서브 에이전트 리뷰는 실제로 실행되었습니다.</sub>

</div>

## 설치

위 명령은 체크섬으로 검증한 `codewhale`과 짧은 별칭 `codew`를 `~/.local/bin`에 넣습니다. 그 뒤 셸이 `command not found`라고 하면 설치 프로그램이 출력하는 PATH 안내를 따르세요([자세히](../INSTALL.md#put-it-on-your-path)). `codewhale update`로 업그레이드합니다.

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

미리 빌드된 바이너리는 Linux, macOS, Windows의 x64와 arm64를 지원합니다. Scoop, Nix, Linux의 Homebrew, Android와 Termux(미리 보기), 수동 다운로드는 [설치 가이드](../INSTALL.md)에, 컨테이너는 [Docker](../DOCKER.md)에 있습니다. 컴퓨터 한 대에는 설치 방법을 하나만 쓰세요. 그렇지 않으면 `PATH`에 사본 두 개가 생기고 앞에 있는 쪽이 실행됩니다.

## 빠른 시작

1. 작업을 커밋하거나 stash한 다음, git 저장소 루트에서 `codewhale`을 실행하세요. 기본적으로 저장소 안의 파일 편집은 묻지 않고 적용하며, 읽기 전용이 아닌 셸 명령은 실행하기 전에 매번 묻습니다.
2. `/provider`를 실행(또는 `F3`)하고 제공자를 고른 뒤 API 키를 붙여 넣고 모델을 선택하세요. 기본 제공자는 DeepSeek입니다. 키 없이 처음 실행하면 첫 메시지를 보낼 때 제공자를 묻고, 로컬 Ollama 서버가 응답하면 로컬 모델을 제안합니다.
3. 작업을 입력하세요:

```text
Fix the failing tests and explain what changed.
```

읽기 전용 명령은 바로 실행됩니다. 그 밖의 셸 명령은 먼저 묻습니다. `y`는 이번 한 번 실행, `a`는 이 대화 동안 비슷한 명령 허용, `n`은 거절입니다. `/undo`는 마지막 요청과 그 파일 변경을 되돌리고, `/help`는 명령과 키를 보여 줍니다.

스크립트와 CI에서는 `codewhale exec`가 인터페이스 없이 작업 하나를 실행합니다. `--auto`로 도구를 주기 전에 [헤드리스 실행](../INSTALL.md#headless-scripts-ci)을 읽어 보세요. 샌드박스를 추가하지 않으면 `--auto`는 Full Access를 쓰기 때문입니다.

## 하는 일

Codewhale에 작업을 주면 작업이 끝나거나 입력이 필요할 때까지 저장소를 읽고, 파일을 편집하고, 명령을 실행합니다. [최신 릴리스 노트](https://github.com/codewhale-hq/Codewhale/releases/latest)에 최신 변경 사항이 있고, [변경 기록](../../CHANGELOG.md)에는 모든 버전이 있습니다.

### 어떤 모델이든

Anthropic, OpenAI, DeepSeek, Google, Mistral, Moonshot, OpenRouter, xAI를 비롯해 40개가 넘는 제공자가 내장되어 있습니다. OpenAI 호환 엔드포인트라면 무엇이든 쓸 수 있고, Ollama, vLLM, SGLang을 통한 로컬 모델도 됩니다. 기본 모델은 DeepSeek의 `deepseek-flash`이며, 지금은 DeepSeek V4.1 Flash가 응답합니다([자세히](../PROVIDERS.md#deepseek-v41-flash)).

`/model`은 턴 사이에 모델을 바꾸고, `/model auto`는 턴마다 모델을 고릅니다(고르는 방식은 `/router`로 설정). `/turn inspect`는 어떤 턴이 어떤 모델을 썼고 비용이 얼마였으며 시간이 얼마나 걸렸는지 보여 줍니다. [제공자](../PROVIDERS.md)를 참고하세요.

### 영수증과 되돌리기

`/receipts`(종료 후에는 `codewhale receipts --last`)는 세션이 바꾼 파일, 실행한 명령, 웹과 MCP 호출, 시작한 에이전트, 각 승인과 승인한 사람, 실패한 작업을 보여 줍니다. `/undo`는 마지막 요청과 그 요청이 바꾼 파일을 한 번에 되돌립니다. `/restore`는 작업 공간 전체를 이전 스냅숏으로 되돌리는데, 이후 편집을 덮어쓰므로 `/trust on` 또는 Full Access가 필요합니다. [영수증](../RECEIPTS.md)을 참고하세요.

### 하나의 작업에 여러 에이전트

`/goal`은 여러 턴에 걸쳐 이어지는 목표를 정합니다. Codewhale은 작업을 [서브 에이전트](../SUBAGENTS.md)에게 나누거나, 각자의 역할과 모델을 가진 에이전트 [Fleet](../FLEET.md)를 실행하거나, 저장소에 커밋된 [워크플로](../WORKFLOW_AUTHORING.md)를 따를 수 있습니다.

### 확장

[MCP 서버](../MCP.md)를 연결하고, [스킬](../SKILLS.md)과 [플러그인](../PLUGINS.md)을 추가하고, 세션과 도구 이벤트에서 [훅](../HOOKS.md)을 실행하세요. 코드 모드(실험적, 기본 사용)는 모델이 여러 MCP 도구와 플러그인 도구를 호출하는 `execute_tools` 프로그램 하나를 작성하게 합니다. 각 호출은 여전히 승인 규칙을 따르고 영수증을 남깁니다. v0.10.2에 포함된 Computer Use 플러그인(실험적, macOS 전용)은 다른 앱을 관찰하고 조작할 수 있으며, 접근 권한을 검토하고 켜기 전까지는 꺼져 있습니다([가이드](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/crates/tui/plugins/computer-use/README.md)). 0.10.3 예정: 이 플러그인은 [별도로 릴리스](https://github.com/codewhale-hq/codewhale-cu-plugin)됩니다.

### 실행 방법

| 명령 | 하는 일 |
| --- | --- |
| `codewhale` | 대화형 터미널 앱 |
| `codewhale exec "…"` | 인터페이스 없이 작업 하나를 실행하며, 스크립트와 CI용입니다([헤드리스 실행](../INSTALL.md#headless-scripts-ci)) |
| `codewhale web` | `127.0.0.1`에서 동작하는 로컬 웹 클라이언트([가이드](../WEB.md)) |
| `codewhale review --pr N` | GitHub 풀 리퀘스트에 대한 권고성 리뷰입니다. [GitHub 봇 설정](../GITHUB_ACTION.md#set-up-your-own-bot)으로 당신의 모델 키를 써서 실행할 수 있습니다 |
| `codewhale app-server --http` | [Runtime API](../RUNTIME_API.md). 스레드, 이벤트, 승인, 턴마다의 파일 변경을 다루는 로컬 HTTP API |
| `codewhale serve --acp` | stdio를 통한 Agent Client Protocol로, Zed 같은 편집기용입니다 |

터미널 앱, `exec`, `web`, Runtime API는 하나의 엔진을 함께 씁니다([아키텍처](../ARCHITECTURE.md)). 커뮤니티가 관리하는 [VS Code 확장](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)은 사이드바에서 연결됩니다([소스](https://github.com/HengQuWorld/CodeWhale-VSCode)).

## 안전을 지키는 방법

두 개의 스위치가 Codewhale이 묻지 않고 할 수 있는 범위를 정합니다. 입력창이 비어 있을 때 `Tab`은 모드를 바꿉니다. **Plan**은 읽고 검색만 하며 아무것도 바꾸지 않고, **Work**는 파일을 편집하고 명령을 실행하며, **Operate**는 Work와 같은 권한으로 목표가 검증되거나 사용자의 입력이 필요할 때까지 계속 작업합니다. `Shift+Tab`은 권한을 바꿉니다. 기본값인 **Ask**는 저장소 안의 파일 편집을 묻지 않고 적용하고, 읽기 전용이 아닌 셸 명령 전에는 묻습니다. **Auto-Review**는 규칙과 검토 모델이 일상적인 호출을 승인하게 하고 위험도가 높은 호출은 거부합니다. 그런 호출을 직접 승인하려면 Ask로 바꾸세요. **Full Access**는 절대 묻지 않고 Codewhale의 샌드박스를 끄지만, 저장소 규칙과 관리 정책은 계속 적용됩니다. 자세한 내용은 [v0.10.2 가이드](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/docs/MODES.md)에 있습니다.

0.10.3 예정: `Shift+Tab`은 Ask, Accept Edits, Plan, Auto-Review, Full Access를 차례로 거치는 하나의 다이얼이 되고, `Tab`은 더 이상 모드를 바꾸지 않으며, Operate는 폐지됩니다. Ask는 모든 파일 편집 전에도 묻게 되며, 새 기본값인 Accept Edits는 git 저장소 안의 편집을 적용하고 명령 전에는 계속 묻습니다. [현재 가이드](../MODES.md)에서 설명합니다.

Ask와 Auto-Review에서는 시스템에 OS 샌드박스가 있으면 그 안에서 명령을 실행합니다. macOS에서는 Seatbelt, Linux에서는 bubblewrap이 설치되어 있을 때 bubblewrap입니다. Windows에는 아직 OS 샌드박스가 없고, Full Access는 샌드박스를 끕니다. 샌드박스 안에서는 셸 명령과 터미널 세션이 `~/.ssh`, `~/.aws`, `.env` 파일 같은 자격 증명 저장소를 읽을 수 없습니다. `/preview-request`는 다음 요청을 보내지 않고 민감한 정보를 가린 미리 보기를 보여 줍니다.

계정은 필요 없고, `/provider`로 추가한 키는 당신의 컴퓨터에만 남습니다. 자신의 API 키를 쓰면 제공자에게 직접 비용을 내고, 로컬 모델은 키가 필요 없습니다. 가격을 알 때는 바닥글과 `/cost`가 세션 비용을 보여 주고, 모르는 가격은 무료가 아니라 알 수 없음으로 표시합니다.

원격 측정은 기본으로 켜져 있습니다. 집계된 수치(버전과 플랫폼, 세션 길이와 결과, 사용한 내장 제공자, 기능과 오류 횟수)를 90일마다 바뀌는 무작위 설치 ID와 함께 보내며, 대화, 코드, 프롬프트, 모델 이름, 파일이나 저장소 이름, 자격 증명은 절대 보내지 않습니다. 다음 명령으로 끌 수 있습니다:

```bash
codewhale config set telemetry false
```

Codewhale은 시작할 때 GitHub에서 새 릴리스도 확인합니다. 건너뛰려면 `CODEWHALE_NO_UPDATE_CHECK=1`을 설정하세요. 더 많은 내용은 [승인 순서](../AUTHORIZATION_ORDER.md), [샌드박스](../SANDBOX.md), [원격 측정](../TELEMETRY.md)에 있습니다.

## 앞으로의 방향

Codewhale은 터미널, 데스크톱 앱, 브라우저, 클라우드 컴퓨터를 아우르는 하나의 환경이 되어 가고 있으며, 모두 하나의 엔진으로 돌아갑니다. 아래의 각 부분은 오늘 어느 단계에 있는지 알려 줍니다.

| 부분 | 상태 | 오늘 |
| --- | --- | --- |
| 터미널 앱 | 릴리스됨 | macOS, Linux 또는 Windows에 설치하고 아무 저장소에서나 `codewhale`을 실행하세요. |
| 로컬 브라우저 클라이언트 | 릴리스됨 | `codewhale web`을 실행하면 브라우저에서 대화를 읽고, 작업을 보내고, 승인에 응답할 수 있습니다. 터미널과 함께 제공되며 계정이 필요 없습니다. |
| 하나의 작업에 여러 에이전트 | 릴리스됨 | 서브 에이전트, Fleet, 워크플로를 통해 여러 에이전트가 각자의 역할로 하나의 작업을 나눕니다. |
| 데스크톱 앱 | 개발 중, 다운로드 없음 | 같은 엔진 위에 데스크톱 앱을 만들고 있으며, 아직 공개 다운로드는 없습니다. |
| 클라우드 컴퓨터 | 초대 전용 | 클라우드 컴퓨터는 초대 전용이며, 세션이 클라우드의 컴퓨터에서 계속 작업할 수 있도록 만들고 있습니다. |
| 이 모두를 잇는 하나의 세션 | 구축 중 | 터미널에서 데스크톱, 브라우저, 클라우드까지 당신과 함께 이동하며 끝까지 당신의 것으로 남는 하나의 세션을 만들고 있습니다. |

[로드맵](../ROADMAP.md)에는 각 릴리스가 다루는 내용과 도울 수 있는 곳이 있습니다.

## 다른 에이전트에서 옮겨 온다면

Codewhale은 `AGENTS.md`를 스스로 읽습니다. `CLAUDE.md`, `.cursorrules`, `.github/copilot-instructions.md`처럼 다른 도구용으로 쓴 지침 파일은 설정에서 `project_instruction_imports = ["claude"]`(또는 `"cursor"`, `"cline"`, `"windsurf"`, `"copilot"`, `"all"`)로 직접 켰을 때만 읽습니다. [config.example.toml](../../config.example.toml)을 참고하세요. 읽지 않은 파일을 발견하면 Codewhale이 경고합니다.

`/import-claude`는 Claude Code 설정과 MCP 서버를 가져올 준비를 해서 검토할 수 있게 합니다. `/plugin install`은 스킬, 명령, 에이전트 프로필, MCP 서버가 담긴 Claude Code 플러그인을 받아들이고, 훅이나 LSP 서버가 필요한 플러그인은 거절합니다([자세히](../CLAUDE_PLUGIN_COMPAT.md)).

## 문서

이 가이드는 메인 브랜치를 따르므로 최신 릴리스에 아직 없는 변경 사항을 설명할 수 있습니다.

| 여기서 시작 | 참고 자료 |
| --- | --- |
| [사용자 가이드](../GUIDE.md) | [설정](../CONFIGURATION.md) |
| [설치](../INSTALL.md) | [아키텍처](../ARCHITECTURE.md) |
| [제공자와 로컬 모델](../PROVIDERS.md) | [Runtime API](../RUNTIME_API.md) |
| [모드와 권한](../MODES.md) | [플러그인 작성](../PLUGIN_AUTHORING.md) |
| [키 바인딩](../KEYBINDINGS.md) | [영수증](../RECEIPTS.md) |
| [GitHub 풀 리퀘스트 리뷰](../GITHUB_ACTION.md) | [전체 문서](../README.md) |

## 기여하기

Codewhale은 쓰는 사람들이 여러 언어로 공개적으로 만듭니다.

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="Codewhale 기여자 벽. Codewhale에 코드, 공동 저자 커밋, 이어받은 풀 리퀘스트, 크레디트가 남은 보고가 있는 사람마다 아바타가 하나씩 있고, 맨 위에 전체 인원이 있습니다."></a>

[기여자 벽](../CONTRIBUTORS.md)은 Codewhale에 코드, 공동 저자 커밋, 이어받은 풀 리퀘스트, 크레디트가 남은 보고가 있는 모든 사람을 세며, GitHub 자체 기여자 목록이 놓치는 사람도 포함합니다. `scripts/contributor-wall.py`가 git 기록과 GitHub로 그림과 목록을 다시 만듭니다.

버그, 아이디어, 보기나 쓰기에 어색했던 점 등 어떤 피드백이든 환영합니다. 모두 [이슈를 열어](https://github.com/codewhale-hq/Codewhale/issues/new/choose) 주세요. 새 기능을 시작하기에도 이슈가 가장 좋습니다. 수정, 번역, 제공자, 계획된 작업에 대한 풀 리퀘스트도 환영합니다. 질문은 [Discussions](https://github.com/codewhale-hq/Codewhale/discussions)에 올려 주세요. [기여 가이드](../../CONTRIBUTING.md)는 도움의 종류별로 어디로 가야 하는지, 당신의 이름으로 작업이 반영되는 방식, [무엇이 어디에 있는지](../../CONTRIBUTING.md#project-structure)를 알려 줍니다.

[Discord](https://discord.gg/37gfS3ksug)에서 이야기하거나, 메인테이너 Hunter를 WeChat(`hunterbown`)에서 추가하고 Whale Brothers 그룹에 들어가고 싶다고 말해 주세요. 누구나 환영합니다.

## 역사와 라이선스

Codewhale은 `deepseek-tui`로 시작했으며 지금도 그 프로젝트의 설정과 세션을 읽습니다. 독립적으로 유지 관리되며 DeepSeek이나 다른 모델 제공자와 제휴 관계가 없습니다.

[MIT](../../LICENSE). 다른 오픈 소스 프로젝트에서 가져온 코드는 제3자 고지의 [포함된 소스](../../THIRD_PARTY_NOTICES.md)와 [가져온 설계](../THIRD_PARTY_NOTICES.md)에 나와 있습니다.
