import type { ComputerUseDict } from "../types";

/**
 * Korean dictionary for `app/[locale]/computer-use/page.tsx` and the
 * Computer Use section of the install page. Product names, the menu items
 * Pause, Stop and Check for updates, and the macOS setting names stay as
 * the app shows them; the macOS names use the Korean system UI labels
 * (손쉬운 사용, 화면 기록, 응용 프로그램).
 */
export const computerUse: ComputerUseDict = {
  metaTitle: "Computer Use · 미리 보기 · Codewhale",
  metaDescription: "Mac용 Codewhale Computer Use를 다운로드하고 설정하세요. 백그라운드 앱 제어, 권한 설정, 그리고 사용자가 직접 누르는 Pause와 Stop 제어를 제공합니다.",
  title: "Computer Use",
  lead: "컴퓨터를 계속 사용하면서 에이전트가 Mac 앱에서 작업하도록 하세요.",
  publisher: "제공: Codewhale",
  download: "Mac용 다운로드",
  downloadZip: "ZIP 아카이브(앱 내 업데이터가 사용)",
  requirements: "macOS 13.5 이상 · Apple silicon 및 Intel",
  included: "앱 하나만 다운로드하면 됩니다. Node나 컴파일러를 따로 설치할 필요가 없습니다.",
  pendingTitle: "Mac용 다운로드 준비 중",
  pendingBody: "Apple 공증과 릴리스 검사가 끝나면 공개 설치 파일이 여기에 표시됩니다.",
  unavailableTitle: "다운로드 가능 여부를 확인할 수 없습니다",
  unavailableBody: "페이지를 새로 고쳐 다시 시도하거나 아래의 공개 릴리스를 확인하세요.",
  releases: "공개 릴리스",
  receipt: "다운로드 검증 정보",
  setup: "Mac 설정하기",
  steps: [
    { title: "앱 설치", body: "디스크 이미지를 열고 Codewhale Computer Use를 “응용 프로그램”으로 드래그하세요. “응용 프로그램”에서 앱을 연 다음 메뉴 막대의 고래 아이콘에서 Computer Use를 선택합니다." },
    { title: "권한 검토", body: "설정 버튼으로 시스템 설정의 “손쉬운 사용”과 “화면 기록”을 여세요. 어떤 권한을 허용할지는 당신이 결정합니다." },
    { title: "에이전트 연결", body: "CLI 버전에 맞는 Codewhale 설정을 아래에서 선택하세요. 다른 호환 MCP 호스트는 저장소의 설정 안내를 따르세요." },
  ],
  controlsTitle: "언제든 직접 제어",
  controlsBody: "macOS 권한을 허용하면 표준 모드는 앱마다 허용 여부를 묻지 않고 작동합니다. 차단한 앱에는 접근할 수 없습니다. Pause는 컴퓨터 사용을 일시 중지하고 Stop computer use는 현재 세션을 종료합니다. 엄격한 승인 모드도 선택할 수 있습니다. 작업을 지켜보고 싶을 때 실시간 미리보기를 요청하세요.",
  updateTitle: "업데이트 시점은 당신이 결정",
  updateBody: "앱에서 Check for updates(업데이트 확인)를 선택하세요. 업데이트를 설치하기 전에 다운로드 파일, Codewhale 서명, Apple 공증을 검사하고 복구를 위해 이전 앱을 보관합니다.",
  help: "설정 및 문제 해결",
  notes: "릴리스 노트",
  demo: "백그라운드 검사 보기",
  source: "GitHub에서 보기",
  platforms: "Mac 도우미는 베타이며 마켓플레이스 플러그인은 Mac만 지원합니다. 공개 릴리스에는 서명되지 않은 실험용 Windows x64 프리뷰도 있습니다. Linux는 소스 또는 Docker를 통한 실험적 설정이 필요합니다. Windows와 Linux의 입력은 공유 데스크톱을 사용합니다.",
  connectTitle: "Codewhale 연결",
  connectVersion: "Codewhale 0.10.2에는 이전 Computer Use 플러그인이 내장되어 있습니다. 카탈로그 항목을 설치해도 이 플러그인은 업데이트되지 않습니다. 설치된 Mac 도우미에 MCP로 직접 연결하세요.",
  connectLimit: "MCP 직접 연결은 도구만 추가하며 플러그인 스킬은 포함하지 않습니다. 이전 내장 플러그인과 함께 사용하지 마세요. 해당 플러그인이 켜져 있다면 먼저 /plugin disable computer-use를 실행하세요.",
  connectRuntime: "앱에 Node가 포함되어 있어 별도 설치가 필요 없습니다. 다른 위치에 설치했다면 APP를 실제 앱 번들 경로로 설정하세요.",
  connectReload: "실행 중인 터미널 세션에서 /mcp reload를 실행한 뒤 다음과 같이 요청하세요:",
  connectPrompt: "request_access로 Computer Use 권한을 확인하세요. 입력은 보내지 마세요.",
  connectExpected: "응답에서 via: app, 도우미 버전, 누락된 권한을 확인하세요. 도우미가 없다면 중단하고 설정 안내를 따르세요. 이 단계는 연결과 권한만 확인합니다.",
  connectGuide: "MCP 설정 안내",
  contributeTitle: "Computer Use 개선에 참여하기",
  contributeBody: "문서를 개선하거나 앱 작업 흐름을 테스트하고 재현 가능한 결과를 공유해 주세요.",
  contributeGuide: "기여 안내",
  contributeIssue: "문제 신고",
};
