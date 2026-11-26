<!-- source: README.md sha256:308ec0b2a2e8 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/wordmark-inverted.svg">
  <img src="../../brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**터미널용 오픈 소스 Rust 코딩 에이전트. 모든 모델, 멀티 에이전트 Fleet, OS 샌드박스, 검증 가능한 영수증을 지원합니다.**

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[웹사이트](https://codewhale.net) · [문서](../README.md) · [변경 기록](../../CHANGELOG.md) · [기여하기](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<sub>이 번역은 기계 번역 초안이며 원어민 검토를 아직 받지 않았습니다.</sub>

<img src="../../web/public/codewhale-tui-latest.png" width="760" alt="로컬 Ollama (qwen3:4b)로 실제 작업을 마친 터미널의 Codewhale v0.10.4: ls.py를 편집하고 python3 -m unittest -v test_ls.py 실행 전 승인을 기다린 뒤 2개의 테스트를 통과시켜 결과를 보고합니다.">

<sub>배포된 Codewhale v0.10.4 (48669ad108fa)에서 로컬 Ollama (qwen3:4b)를 Accept Edits 모드로 실행한 실제 세션.</sub>

</div>

## 설치

위 설치 스크립트는 macOS와 Linux에서 체크섬이 검증된 `codewhale`과 짧은 별칭 `codew`를 `~/.local/bin`에 설치합니다. 셸에서 `command not found`가 표시되면 `~/.local/bin`을 `PATH`에 추가하세요([자세히](../INSTALL.md#put-it-on-your-path)). `codewhale update`로 바로 업그레이드할 수 있습니다.

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

사전 빌드 바이너리는 Linux, macOS, Windows(x64 및 arm64)를 지원합니다. Scoop, Nix, Linux용 Homebrew, Android/Termux(프리뷰), 수동 다운로드는 [docs/INSTALL.md](../INSTALL.md), 컨테이너 이미지는 [docs/DOCKER.md](../DOCKER.md)에 안내되어 있습니다. `PATH`에서 바이너리가 충돌하지 않도록 머신당 하나의 설치 방법만 사용하세요.

## 빠른 시작

1. 작업 내용을 커밋하거나 스태시한 뒤 git 저장소 루트에서 `codewhale`을 실행합니다.
2. `/provider`(또는 `F3`)를 실행해 제공자를 선택하고 API 키를 붙여넣은 뒤 모델을 고릅니다. 기본값은 DeepSeek이며, 키가 없고 로컬 Ollama 서버가 응답하면 첫 메시지에서 로컬 모델을 제안합니다.
3. 작업을 입력합니다:

```text
Fix the failing tests and explain what changed.
```

읽기 전용 명령은 즉시 실행됩니다(Ask에서는 모든 명령 전에 묻습니다). 상태를 바꾸는 셸 명령은 실행 전에 승인을 묻습니다(`y` 한 번 실행, `a` 현재 세션에서 유사 명령 허용, `n` 거부). `/undo`로 직전 턴과 파일 변경을 되돌리고, `/help`로 명령과 단축키를 확인할 수 있습니다.

## 주요 기능

- **50개 이상의 내장 제공자와 로컬 모델:** Anthropic, OpenAI, DeepSeek(기본 `deepseek-flash` / V4.1 Flash, [자세히](../PROVIDERS.md#deepseek-v41-flash)), Google, Mistral, Moonshot, OpenRouter, xAI, 모든 OpenAI 호환 엔드포인트와 Ollama, vLLM, SGLang 로컬 런타임을 지원합니다. `/model`로 세션 중 모델을 전환하고, `/model auto`(`/router`)로 턴마다 라우팅하며, `/turn inspect`로 지연 시간과 비용을 확인합니다. [제공자 문서](../PROVIDERS.md)를 참고하세요.
- **영수증과 롤백:** `/receipts`(종료 후 `codewhale receipts --last`)는 변경된 파일, 실행한 명령, 웹/MCP 호출, 서브 에이전트, 승인 내역, 실패를 기록합니다. `/undo`는 직전 턴을 한 번에 되돌리며, `/restore`는 작업 공간을 이전 스냅샷으로 복원합니다(`/trust on` 또는 Full Access 필요). [영수증 문서](../RECEIPTS.md)를 참고하세요.
- **서브 에이전트, Fleet, 워크플로:** `/goal`로 여러 턴에 걸친 목표를 유지하고, [서브 에이전트](../SUBAGENTS.md)에 작업을 위임하거나 역할·모델별 [Fleet](../FLEET.md)을 조율하고, 저장소에 커밋된 [워크플로](../WORKFLOW_AUTHORING.md)를 실행할 수 있습니다.
- **MCP, 스킬, 훅, 플러그인:** [MCP 서버](../MCP.md), [스킬](../SKILLS.md), [플러그인](../PLUGINS.md), 수명 주기 [훅](../HOOKS.md)을 연결합니다. 실험적 코드 모드(`execute_tools`)는 일반 승인 규칙 아래에서 여러 MCP/플러그인 호출을 묶어 실행합니다. 선택형 macOS [Computer Use](https://github.com/codewhale-hq/codewhale-cu-plugin)는 권한을 검토하고 직접 켜기 전까지 꺼져 있습니다.
- **호환성:** `AGENTS.md`를 자동으로 읽습니다. `CLAUDE.md`, `.cursorrules`, `.github/copilot-instructions.md`는 [config.example.toml](../../config.example.toml)의 `project_instruction_imports`로 선택적으로 불러올 수 있습니다. `/import-claude`로 Claude Code 설정과 MCP 서버를 가져오고, `/plugin install`로 Claude Code 플러그인(스킬, 명령, 에이전트 프로필, MCP)과 DeepSeek Harness (DSH) 플러그인을 설치할 수 있습니다([자세히](../CLAUDE_PLUGIN_COMPAT.md)).

## 실행 방식

| 명령 | 용도 |
| --- | --- |
| `codewhale` | 대화형 터미널 앱 |
| `codewhale exec "…"` | 스크립트 및 CI용 헤드리스 단일 작업 실행기([가이드](../INSTALL.md#headless-scripts-ci); 샌드박스 없이 `--auto` 사용 시 Full Access 적용) |
| `codewhale web` | `127.0.0.1`에서 실행되는 로컬 웹 클라이언트([가이드](../WEB.md)) |
| `codewhale review --pr N` | GitHub PR 참고용 리뷰; [GitHub 봇 설정](../GITHUB_ACTION.md#set-up-your-own-bot)으로 본인 키에서 실행 |
| `codewhale app-server --http` | 스레드, 이벤트, 승인, 턴별 diff를 제공하는 로컬 HTTP [Runtime API](../RUNTIME_API.md) |
| `codewhale serve --acp` | Zed 같은 편집기를 위한 stdio 기반 Agent Client Protocol |

모든 진입점은 하나의 Rust 엔진을 공유합니다([아키텍처](../ARCHITECTURE.md)). 커뮤니티가 유지 관리하는 [VS Code 확장](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)([소스](https://github.com/HengQuWorld/CodeWhale-VSCode))도 같은 엔진에 연결됩니다. 최신 릴리스와 데스크톱/클라우드 현황은 [최신 릴리스 노트](https://github.com/codewhale-hq/Codewhale/releases/latest)와 [로드맵](../ROADMAP.md)을 참고하세요.

## 권한, 샌드박스 및 개인정보 보호

`Shift+Tab`으로 권한 모드를 전환합니다: **Ask → Accept Edits → Plan → Auto-Review → Full Access**(`Tab`은 자동 완성 전용). **Accept Edits**(기본값)는 git 저장소 내 파일 편집을 바로 적용하고 명령 실행 전에 묻습니다. **Ask**는 모든 편집과 명령마다 묻습니다. **Plan**은 읽기 전용입니다. **Auto-Review**는 규칙과 리뷰어 모델로 일상 호출을 승인하고 고위험 작업을 차단합니다. **Full Access**는 확인을 건너뛰고 OS 샌드박스를 끕니다(저장소 규칙과 관리 정책은 유지됨). 자세한 내용은 [모드 문서](../MODES.md)를 참고하세요.

Ask, Accept Edits, Auto-Review에서는 지원하는 OS 샌드박스 안에서 명령을 실행합니다(macOS는 Seatbelt, Linux는 bubblewrap, Windows는 OS 샌드박스 미지원). 샌드박스된 명령과 터미널 세션은 `~/.ssh`, `~/.aws`, `.env` 같은 자격 증명 경로를 읽을 수 없습니다. `/preview-request`로 전송 전 마스킹된 요청을 미리 볼 수 있습니다.

`/provider`로 추가한 API 키는 로컬 머신에만 저장되며 로컬 모델은 키가 필요 없습니다. `/cost`와 바닥글은 가격이 알려진 경우 세션 비용을 추적합니다(알 수 없는 가격은 무료가 아니라 알 수 없음으로 표시).

집계 텔레메트리는 기본적으로 켜져 있습니다(버전, OS, 세션 길이/결과, 내장 제공자 ID, 내장 공개 카탈로그 모델 ID(사용자 지정·비공개·로컬 모델은 `other`로 기록), 총 토큰 수, 내장 도구 호출/오류 수, 기능/오류 횟수; 90일마다 교체되는 임의 설치 ID; 코드, 프롬프트, 대화, 사용자 지정/비공개 모델명, 파일/저장소/브랜치 이름, 자격 증명, IP 주소는 전송하지 않음). 끄려면 다음을 실행하세요:

```bash
codewhale config set telemetry false
```

시작 시 GitHub 새 릴리스 확인(`CODEWHALE_NO_UPDATE_CHECK=1`로 건너뛰기)과 24시간마다 공개 `models.dev` 모델 카탈로그 새로고침도 수행합니다(`CODEWHALE_DISABLE_MODELS_DEV_FETCH=1` 또는 `[catalog] auto_refresh = false`로 건너뛰기; CI 환경 및 `DO_NOT_TRACK`에서는 자동 비활성화). 자세한 내용은 [권한 부여 순서](../AUTHORIZATION_ORDER.md), [샌드박싱](../SANDBOX.md), [텔레메트리](../TELEMETRY.md)를 참고하세요.

## 문서

가이드는 `main` 브랜치를 따르므로 최신 릴리스에 아직 포함되지 않은 변경 사항이 설명될 수 있습니다.

| 시작하기 | 참고 문서 |
| --- | --- |
| [사용자 가이드](../GUIDE.md) | [설정](../CONFIGURATION.md) |
| [설치](../INSTALL.md) | [아키텍처](../ARCHITECTURE.md) |
| [제공자 및 로컬 모델](../PROVIDERS.md) | [Runtime API](../RUNTIME_API.md) |
| [모드 및 권한](../MODES.md) | [플러그인 작성](../PLUGIN_AUTHORING.md) |
| [단축키](../KEYBINDINGS.md) | [영수증](../RECEIPTS.md) |
| [GitHub PR 리뷰](../GITHUB_ACTION.md) | [전체 문서](../README.md) |

## 기여 및 라이선스

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="Codewhale 기여자 목록"></a>

[기여자 목록](../CONTRIBUTORS.md)(`scripts/contributor-wall.py`)은 코드, 공동 작성 커밋, 이어받은 PR, 보고자를 기록합니다. 버그 제보와 기능 제안은 [Issues](https://github.com/codewhale-hq/Codewhale/issues/new/choose), 질문은 [Discussions](https://github.com/codewhale-hq/Codewhale/discussions), PR 안내와 저장소 구조는 [CONTRIBUTING.md](../../CONTRIBUTING.md)([프로젝트 구조](../../CONTRIBUTING.md#project-structure))를 참고하세요. [Discord](https://discord.gg/37gfS3ksug)에 참여하거나 WeChat에서 `hunterbown`을 추가해 Whale Brothers 그룹에 합류할 수 있습니다.

Codewhale은 `deepseek-tui`에서 시작되었으며 기존 설정과 세션의 하위 호환성을 유지합니다. 독립적으로 유지 관리되며 DeepSeek이나 다른 모델 제공자와 제휴 관계가 아닙니다.

[MIT](../../LICENSE) 라이선스를 따릅니다. 서드파티 고지: [포함된 소스](../../THIRD_PARTY_NOTICES.md) 및 [참고한 설계](../THIRD_PARTY_NOTICES.md).
