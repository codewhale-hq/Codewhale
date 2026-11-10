import { projectText } from "../i18n/project";
import type { LocalizedText } from "./vocabulary";
import { REPO_ISSUES_URL, REPO_URL } from "../i18n/links";

const DOCS = `${REPO_URL}/blob/main/docs`;
const MILESTONE = `${REPO_URL}/milestone`;
const DISCUSSIONS = `${REPO_URL}/discussions`;

export interface ProjectLink {
  label: LocalizedText;
  href: string;
}

export interface ProjectGoal {
  title: LocalizedText;
  body: LocalizedText;
  open: LocalizedText;
  links: ProjectLink[];
}

const issue = (number: number): ProjectLink => ({
  label: { en: `Issue ${number}`, zh: `Issue ${number}` },
  href: `${REPO_ISSUES_URL}/${number}`,
});

export const PROJECT_GOALS: ProjectGoal[] = [
  {
    title: projectText("oneEngineBehindEverySurface"),
    body: projectText("theTerminalCodewhaleExecTheLocalBrowser"),
    open: projectText("theLargestCratesStillNeedToBe"),
    links: [issue(5316), issue(4986)],
  },
  {
    title: projectText("everyModelIsAPeer"),
    body: projectText("useABuiltInProviderAnyOpenAI"),
    open: projectText("providerBehaviorStillNeedsToMoveInto"),
    links: [issue(5092), { label: projectText("providerList"), href: `${DOCS}/PROVIDERS.md` }],
  },
  {
    title: projectText("youStayInControl"),
    body: projectText("codewhaleAsksBeforeItActsOutsideWhat"),
    open: projectText("windowsStillHasNoSandboxApprovalPrompts"),
    links: [{ label: projectText("sandboxGuide"), href: `${DOCS}/SANDBOX.md` }],
  },
  {
    title: projectText("manyAgentsWorkInOnePlace"),
    body: projectText("splitAJobAcrossSubAgentsA"),
    open: projectText("otherCodingAgentsCannotRunInsideCodewhale"),
    links: [issue(6013), issue(7206)],
  },
  {
    title: projectText("bringWhatYouAlreadyHave"),
    body: projectText("codewhaleReadsAGENTSMdImportsClaudeCode"),
    open: projectText("deepseekHarnessPluginsAreNotYetFully"),
    links: [{ label: projectText("compatibilityChecklist"), href: `${DOCS}/DSH_PLUGIN_COMPAT.md` }],
  },
  {
    title: projectText("itWorksInYourLanguageOnYour"),
    body: projectText("theTerminalTheREADMEAndThisSite"),
    open: projectText("moreTranslationsAreNeededAndInputMethods"),
    links: [
      { label: projectText("localizationMatrix"), href: `${DOCS}/LOCALIZATION.md` },
      issue(6804),
    ],
  },
  {
    title: projectText("aProjectThatHelpsBuildItself"),
    body: projectText("codewhaleIsUsedToWorkOnCodewhale"),
    open: projectText("memoryAcrossSessionsAndSkillsLearnedWith"),
    links: [issue(6050), issue(5860)],
  },
];

export type ReleaseState = "progress" | "planned" | "later";

export const RELEASE_STATE_LABEL: Record<ReleaseState, LocalizedText> = {
  progress: projectText("inProgress"),
  planned: projectText("planned"),
  later: projectText("later"),
};

export const PROJECT_RELEASES: { name: string; state: ReleaseState; body: LocalizedText; href: string }[] = [
  {
    name: "0.10.3",
    state: "progress",
    body: projectText("thisReleaseFixesProblemsReportedAgainst0"),
    href: `${MILESTONE}/73`,
  },
  {
    name: "0.10.4",
    state: "planned",
    body: projectText("thisReleaseImprovesReliabilityThroughProcessCleanup"),
    href: `${MILESTONE}/75`,
  },
  {
    name: "0.10.5",
    state: "planned",
    body: projectText("thisReleaseImprovesUsabilityWithRunningCommand"),
    href: `${MILESTONE}/76`,
  },
  {
    name: "0.11",
    state: "planned",
    body: projectText("thisReleaseReworksTheArchitectureWithSmaller"),
    href: `${MILESTONE}/72`,
  },
  {
    name: "1.0",
    state: "later",
    body: projectText("thisReleaseBringsTheDesktopAppTo"),
    href: `${MILESTONE}/66`,
  },
];

export const PROJECT_COPY = {
  goalsLabel: projectText("roadmap"),
  goalsHeading: projectText("whereCodewhaleIsGoing"),
  goalsScope: projectText("codewhaleHasSevenLongTermGoalsAnd"),
  goalsListHeading: projectText("sevenLongTermGoals"),
  goalsListScope: projectText("eachOneNamesWhatIsStillOpen"),
  goalsLink: projectText("readTheRoadmap"),
  goalOpen: projectText("stillOpen"),

  releasesHeading: projectText("whatEachReleaseIsFor"),
  releasesScope: projectText("aMilestoneIsAPlanNotA"),

  liveLabel: projectText("liveFromGitHub"),
  liveHeading: projectText("meetThePeopleBuildingCodewhale"),
  liveScope: projectText("codewhaleImprovesThroughThePeopleWhoUse"),
  liveLink: projectText("allActivity"),
  mergedHeading: projectText("justMerged"),
  mergedAge: projectText("mergedAge"),
  firstContribution: projectText("firstContribution"),
  statStars: projectText("stars"),
  statContributors: projectText("contributors"),
  statPulls: projectText("openPullRequests"),
  statIssues: projectText("openIssues"),
  facesAll: projectText("allCountContributors"),
  loopFeedback: projectText("giveFeedback"),
  loopHelp: projectText("seeWhereToHelp"),

  mergedPageHeading: projectText("mergedInTheLastFewDays"),
  mergedPageScope: projectText("eachOfThesePullRequestsMergedUnder"),
  openNowHeading: projectText("pickUpSomethingOpenNow"),
  openNowScope: projectText("theseIssuesCarryTheHelpWantedLabel"),
  milestoneProgress: projectText("closedOfTotalClosed"),

  routesHeading: projectText("findWhereYoursGoes"),
  routesScope: projectText("ifYouAreNotSureOpenAn"),
  featureLabel: projectText("newFeatures"),
  featureHeading: projectText("startAFeatureWithAnIssue"),
  featureScope: projectText("theQuestionsThatDecideAFeatureAre"),
  featureSteps: [
    {
      title: projectText("openAFeatureRequest"),
      body: projectText("sayWhatYouAreTryingToDo"),
    },
    {
      title: projectText("agreeOnTheShape"),
      body: projectText("aMaintainerAnswersWithWhatFitsOr"),
    },
    {
      title: projectText("sendThePullRequest"),
      body: projectText("linkTheIssueInYourPullRequest"),
    },
  ],
  standingHeading: projectText("sendTheseWithoutAsking"),
  standingScope: projectText("theseStandingProjectsAreAlwaysOpenAnd"),
  roadmapHeading: projectText("seeWhereTheProjectIsHeaded"),
  statusLabel: projectText("status"),
};

export interface ContributionRoute {
  have: LocalizedText;
  place: LocalizedText;
  href: string;
}

const NEW_ISSUE = `${REPO_ISSUES_URL}/new`;

export const CONTRIBUTION_ROUTES: ContributionRoute[] = [
  {
    have: projectText("aBugOrBehaviorThatSurprisedYou"),
    place: projectText("bugReport"),
    href: `${NEW_ISSUE}?template=bug_report.yml`,
  },
  {
    have: projectText("anIdeaOrAFeatureYouWant"),
    place: projectText("featureRequest"),
    href: `${NEW_ISSUE}?template=feature_request.yml`,
  },
  {
    have: projectText("feedbackOnHowItLooksReadsOr"),
    place: projectText("feedbackForm"),
    href: `${NEW_ISSUE}?template=feedback.yml`,
  },
  {
    have: projectText("aQuestion"),
    place: projectText("qAOrDiscord"),
    href: `${DISCUSSIONS}/categories/q-a`,
  },
  {
    have: projectText("aSecurityProblem"),
    place: projectText("privateAdvisory"),
    href: `${REPO_URL}/security/advisories/new`,
  },
  {
    have: projectText("aFixATranslationAProviderOr"),
    place: projectText("pullRequest"),
    href: `${REPO_URL}/blob/main/CONTRIBUTING.md#standing-projects`,
  },
  {
    have: projectText("aNewFeature"),
    place: projectText("issueFirst"),
    href: `${NEW_ISSUE}?template=feature_request.yml`,
  },
];

export const STANDING_PROJECTS: { title: LocalizedText; body: LocalizedText; href: string }[] = [
  {
    title: projectText("translations"),
    body: projectText("improveTheTerminalLanguagePacksTheTranslated"),
    href: `${DOCS}/LOCALIZATION.md`,
  },
  {
    title: projectText("providersAndModels"),
    body: projectText("addAProviderOrAModelOr"),
    href: `${DOCS}/PROVIDERS.md`,
  },
  {
    title: projectText("documentation"),
    body: projectText("fixATypoAnUnclearStepOr"),
    href: `${REPO_URL}/tree/main/docs`,
  },
  {
    title: projectText("bugFixes"),
    body: projectText("ifThereIsNoIssueYetDescribe"),
    href: `${REPO_ISSUES_URL}?q=is%3Aissue+is%3Aopen+label%3Abug`,
  },
  {
    title: projectText("plannedWork"),
    body: projectText("pickAnIssueLabelledHelpWantedOr"),
    href: `${REPO_ISSUES_URL}?q=is%3Aissue+is%3Aopen+label%3A%22help+wanted%22`,
  },
];
