import type { ChromeDict } from "../types";

/**
 * Korean chrome dictionary — a native rewrite mirroring the current English
 * direction (bring your own model, runs on your machine); the old
 * "local-first" wordmark tag is intentionally gone.
 *
 * Terminology follows the TUI locale pack (`crates/tui/locales/ko.json`):
 * mode and permission names stay literal (Plan / Work / Operate, Ask /
 * Auto-Review / Full Access), 프로바이더 is "provider", 저장소 is
 * "repository", 추론 is "reasoning", 권한 is "permission". Commands, package
 * names, and GitHub are left as-is per docs/VOICE.md.
 *
 * Nav labels are kept to two–four syllables: they sit in one horizontal
 * masthead row. "FAQ" therefore renders as 질의응답 rather than the longer
 * 자주 묻는 질문, in both the nav and the footer so the two agree.
 */
export const chrome: ChromeDict = {
  navDocs: "문서",
  navContribute: "기여",
  navSignIn: "로그인",
  navDesktop: "데스크톱",

  navFeatures: "기능",
  navModels: "모델",
  navPlugins: "플러그인",

  skipToContent: "본문으로 건너뛰기",

  navPrimaryAria: "기본 탐색",
  navHomeAria: "Codewhale 홈",

  installCta: "설치 →",

  dateLocale: "ko-KR",

  menuOpen: "메뉴 열기",
  menuClose: "메뉴 닫기",
  translationMachineDraft: "이 번역은 기계 번역 초안이며 원어민 검토를 아직 받지 않았습니다.",

  themeAuto: "자동",
  themeLight: "밝게",
  themeDark: "어둡게",
  themeAria: "테마: {mode} (클릭하면 전환)",
  themeTitle: "테마 · 자동 / 밝게 / 어둡게",

  footerProduct: "제품",
  footerProject: "프로젝트",
  footerDocs: "문서",
  footerGuide: "시작 가이드",
  footerInstall: "설치",
  footerModels: "모델",
  footerFaq: "질의응답",
  footerContribute: "기여",
  footerRoadmap: "로드맵",
  footerLicense: "MIT 라이선스",
  footerMerch: "굿즈",
  footerTerms: "이용약관",
  footerPrivacy: "개인정보처리방침",
  footerChangelog: "변경 로그",
  footerCanonicalSource: "공식 소스: ",
  footerReleases: " · 릴리스: ",
  footerReleasesLink: "GitHub 릴리스",
  footerSecurity: "보안",
  footerLegal: "법적 고지",

  switcherLabel: "언어",
  // "(으)로" is the standard Korean UI hedge when the interpolated noun's
  // final consonant is unknown at write time.
  switcherSwitchTo: "{label}(으)로 전환",
  partialBadge: "(일부 번역)",

  stateReleased: "출시됨",
  stateAvailable: "사용 가능",
  stateBeta: "베타",
  stateDev: "개발 빌드",
  stateOpen: "공개됨",
  stateInviteOnly: "초대 전용",
  stateNotOpen: "아직 열리지 않음",
  stateNotPublic: "비공개",
  stateInSource: "소스에 있으며 아직 출시되지 않음",
  numbersAsOf: "{date} 기준",
  downloadMacos: "macOS용 다운로드",
  downloadVersion: "버전 {version}",
  downloadSize: "크기 {size}",
  downloadChecksum: "SHA-256 체크섬",
  downloadSigned: "서명 및 공증 완료",
};
