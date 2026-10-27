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
  lead: "계속 작업하는 동안 Codewhale이 앱에서 일하게 하세요. 이 Mac에서 한 번 허용하고, 원할 때 언제든 컴퓨터 사용을 멈출 수 있습니다.",
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
    { title: "Computer Use 활성화", body: "Codewhale의 Connectors 설정에서 Computer Use를 설치하거나 업데이트한 뒤 검토하고 활성화하세요. 앱에서 작업해 달라고 요청하면 처음 사용할 때 네이티브 도우미가 표시됩니다. Trust until revoked를 선택하면 앱, 모델, 작업이 바뀌어도 신뢰가 유지됩니다." },
  ],
  controlsTitle: "언제든 직접 제어",
  controlsBody: "작업 중인 앱 옆에 작은 Codewhale 제어 메뉴가 표시됩니다. Pause는 컴퓨터 사용을 일시 중지하고, Stop computer use는 현재의 모든 컴퓨터 세션을 종료합니다. Codewhale의 접근 권한을 철회하려면 도우미에서 Turn off를 선택하세요. 작업을 지켜보고 싶을 때는 실시간 미리보기를 요청하세요.",
  updateTitle: "업데이트 시점은 당신이 결정",
  updateBody: "앱에서 Check for updates(업데이트 확인)를 선택하세요. 업데이트를 설치하기 전에 다운로드 파일, Codewhale 서명, Apple 공증을 검사하고 복구를 위해 이전 앱을 보관합니다.",
  help: "설정 및 문제 해결",
  notes: "릴리스 노트",
  demo: "백그라운드 검사 보기",
  source: "소스 및 다른 플랫폼",
  platforms: "이 다운로드는 Mac용입니다. Windows와 Linux는 현재 소스 플러그인과 호스트 측 설정을 사용합니다.",
};
