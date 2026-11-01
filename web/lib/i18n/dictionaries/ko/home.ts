import type { HomeDict } from "../types";

/** Korean home copy: useful work with chosen models and connected tools. */

export const home: HomeDict = {
  metaTitle: "Codewhale: 오픈 소스 코딩 에이전트",
  metaDescription: "Codewhale은 터미널에서 실행되는 오픈 소스 코딩 에이전트입니다. 선택한 클라우드 또는 로컬 모델로 프로젝트를 읽고, 수정하고, 테스트하세요.",
  heroTitle: "모든 모델을 위한 오픈 소스 코딩 에이전트",
  heroIntro: "{brand}은 터미널에서 프로젝트를 읽고, 파일을 편집하고, 테스트를 실행합니다. 호스팅 또는 로컬 모델을 연결하고 승인이 필요한 작업을 선택하세요.",
  getCodewhale: "Codewhale 설치",
  heroInstallAria: "설치 명령",
  exploreProduct: "Codewhale 둘러보기",
  shotPreview: "터미널 미리보기",
  screenshotAlt:
    "Codewhale v{version}: 대화, 메시지 입력창, 세션 제어를 보여 주는 실제 터미널 캡처.",
  latestRelease: "최신 릴리스 {tag}",
  releaseUnavailable: "릴리스 상태를 확인할 수 없음",
  proofContributors: "기여자 {count}명",
  currentSource: "소스",
  sourceCandidate: "미공개",
  publishedRelease: "공개됨",
  gainHeading: "터미널에서 개발하고 자동화하세요",
  gainLede: "만들거나 자동화하고 싶은 작업을 설명하세요. Codewhale은 사용자가 제어하는 접근 권한 안에서 파일을 편집하고, 명령을 실행하고, 결과를 확인할 수 있습니다.",
  gain: [
    [
      "앱과 도구 만들기",
      "앱을 만들거나, 기능을 추가하거나, 스크립트를 작성하세요. Codewhale은 프로젝트 파일을 다루고, 코드를 실행하고, 만든 결과물을 테스트할 수 있습니다."
    ],
    [
      "반복하는 업무 자동화",
      "터미널, 스크립트 또는 CI에서 워크플로를 실행하세요. 작업이 크다면 서로 다른 모델을 사용하는 Fleet 에이전트 팀에 일부 작업을 맡길 수 있습니다."
    ],
    [
      "쓰고 있는 도구 연결",
      "플러그인과 MCP 서버를 통해 도구를 추가하거나, 자신의 스크립트에서 API를 사용하세요. 각 서비스에는 개별 설정과 인증이 필요합니다."
    ]
  ],
  chapterModels: "당신의 모델",
  modelsHeading: "원하는 모델을 사용하세요",
  modelsBody:
    "제공업체 계정, OpenAI 호환 엔드포인트 또는 로컬 및 자체 호스팅 모델을 연결하세요. 세션과 Fleet의 각 에이전트에 사용할 모델을 선택하세요.",
  modelsFacts: [
    [
      "내 API 계정",
      "자신의 키로 DeepSeek, OpenAI, Anthropic, Google 등의 제공업체에 연결하세요. 기본 모델인 deepseek-flash는 이제 DeepSeek-V4.1-Flash로 실행됩니다."
    ],
    [
      "내 게이트웨이",
      "OpenAI 호환 엔드포인트를 연결하고 제공되는 모델을 선택하세요."
    ],
    [
      "내 추론 환경",
      "Ollama, vLLM, SGLang으로 로컬 또는 자체 호스팅 모델을 실행하세요."
    ],
    [
      "내 ChatGPT 요금제",
      "codewhale auth chatgpt로 로그인하여 API 키 대신 ChatGPT 요금제를 사용하세요. 실험 기능입니다."
    ]
  ],
  modelsLink: "모델과 제공업체 둘러보기",
  startHeading: "시작하기",
  startLede: "Codewhale을 설치하고, 모델을 연결하고, 프로젝트 폴더를 여세요. 필요에 따라 플러그인과 더 많은 에이전트를 추가할 수 있습니다.",
  startGuideLink: "시작 가이드 따라 하기",
  startVocabularyLink: "제품 용어 보기",
  chapterAvailability: "실행 환경",
  availabilityHeading: "현재 제공되는 기능과 개발 중인 기능",
  availabilityLede: "터미널과 로컬 브라우저 클라이언트는 지금 사용할 수 있습니다. 네이티브 데스크톱 앱과 새로 구축하는 호스팅 웹 앱은 개발 중입니다.",
  availability: [
    [
      "터미널과 로컬 브라우저",
      "출시됨",
      "Linux, macOS, Windows에 설치한 뒤 codewhale을 실행하거나, 로컬 브라우저 클라이언트를 쓰려면 codewhale web을 실행하세요. npm과 Cargo로도 설치할 수 있으며, Android의 Termux 버전은 미리보기입니다."
    ],
    [
      "CodeWhale GUI (VS Code)",
      "사용 가능",
      "커뮤니티가 관리하는 별도 프로젝트입니다. 같은 Codewhale Runtime 위에서 VS Code 사이드바로 대화, 스레드, 파일 변경을 다룹니다. VS Code Marketplace에서 설치하세요.",
      "https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode"
    ],
    [
      "호스팅 웹 앱",
      "개발 중",
      "데스크톱 앱에 맞춰 다시 만드는 중이며 아직 공개되지 않았습니다. 지금 브라우저에서 사용하려면 자신의 컴퓨터에서 codewhale web을 실행하세요."
    ],
    [
      "데스크톱",
      "개발 빌드",
      "같은 엔진으로 실행되는 네이티브 앱입니다. 변경 사항을 검토하고 커밋하며, 여러 프로젝트의 작업을 확인하고, 메시지를 편집해 새 분기로 다시 보낼 수 있습니다. 아직 공개 다운로드는 없습니다."
    ]
  ],
  availabilityNote: "터미널, 로컬 브라우저, GUI는 Codewhale 계정이 필요 없습니다. 본인의 제공업체 키를 사용하는 경우, 해당 사용 요금은 그 제공업체가 청구합니다.",
  surfacesHeading: "파일과 도구로 작업하세요",
  surfaces: [
    [
      "파일과 터미널",
      "파일을 만들고, 명령을 실행하고, 데이터를 살펴보고, 만든 것을 테스트하세요. 작업 폴더와 권한은 직접 설정합니다."
    ],
    [
      "플러그인과 연결된 앱",
      "플러그인과 MCP로 스킬과 도구를 추가하세요. 에이전트가 사용할 연결을 검토하고 활성화합니다."
    ],
    [
      "브라우저와 컴퓨터 사용 · 미리보기",
      "허용한 접근 범위에서 브라우저 도구와 Computer Use 플러그인으로 앱과 웹사이트에서 작업하세요."
    ],
    [
      "이어서 할 수 있는 세션",
      "대화, 도구 결과, 작업 기록을 함께 보관하세요. 터미널이나 로컬 브라우저 클라이언트에서 작업을 다시 이어 갑니다."
    ],
    [
      "에이전트 팀",
      "Fleet으로 큰 일을 역할과 모델이 다른 에이전트에게 나누고, 한곳에서 진행 상황을 확인하세요."
    ]
  ],
  runtimeLink: "도구와 통합 살펴보기",
  installBandHeading: "macOS 또는 Linux에 설치하세요",
  copy: "복사",
  copied: "복사됨 ✓",
  binaries: "바이너리",
  chinaMirrors: "중국 미러",
  installGuideLink: "설치 가이드 읽기",
  communityHeading: "Codewhale에 기여하세요",
  communityBody: "{count}명이 Codewhale 개발에 참여했습니다. GitHub에서 버그를 신고하거나, 문서를 개선하거나, 코드를 기여하세요.",
  communityLinksAria: "커뮤니티 링크",
  contribute: "GitHub에서 기여하기",
};
