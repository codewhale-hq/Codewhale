import type { ContributeDict } from "../types";

/** Korean dictionary for `app/[locale]/contribute/page.tsx`. */
export const contribute: ContributeDict = {
  metaTitle: "기여하기 · Codewhale",
  metaDescription:
    "코드가 있든 없든 모든 피드백이 Codewhale에 도움이 됩니다. 버그, 아이디어, 번역, 공급자, 수정 사항을 어디에 보내야 하는지 확인해 보세요.",
  kicker: "Codewhale을 함께 만들어요",
  title: "Codewhale 만들기에 함께하세요",
  lede: "코드가 없어도 모든 피드백이 도움이 됩니다. 버그 하나, 아이디어 하나, 이상해 보였던 화면 하나만으로도 시작하기에 충분합니다.",
  fileIssue: "이슈 열기",
  browsePulls: "pull request 보기",
  fullGuide: "기여 안내",
  pathsTitle: "시작할 곳 고르기",
  workflowTitle: "다른 사람이 검토하기 쉬운 변경 보내기",
  reviewTitle: "무엇을 바꿨고 어떻게 확인했는지 보여 주기",
  reviewScope:
    "문제, 변경 이유, 테스트 근거, 남은 위험을 검토자에게 알려 주세요. 범위가 좁을수록 피드백도 더 분명해집니다.",
  devTitle: "코드 변경을 빌드하고 확인하기",
  devScope:
    "Rust 코드를 수정했다면 관련 코드를 빌드하고 해당 부분에 맞춘 검사를 실행하세요. 변경 내용에 따라 더 넓은 검사가 필요하면 기여 안내를 따르세요.",
  mergedAccountsCount: {
    other: "지난 30일 동안 GitHub 계정 {n}개의 pull request가 병합되었습니다.",
  },
  mergedPeopleCount: {
    other: "지난 30일 동안 {n}명의 pull request가 병합되었습니다.",
  },
};
