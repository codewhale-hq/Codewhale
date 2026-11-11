export const CONSTITUTION_LAYERS = [
  {
    n: "01",
    name: { en: "Bundled Constitution", zh: "内置宪章" },
    path: { en: "compiled into every binary", zh: "编译进每一个二进制" },
    body: {
      en: "The base rules. Its priority article fixes the authority order for any conflict, so a stale handoff can never outrank a fresh test result by accident.",
      zh: "基础规则。其中的位阶条款为一切冲突固定裁决顺序——过期的交接不会稀里糊涂地压过刚跑出的测试结果。",
    },
  },
  {
    n: "02",
    name: { en: "/constitution — your standing rules", zh: "/constitution——你的常备规则" },
    path: { en: "$CODEWHALE_HOME/constitution.json", zh: "$CODEWHALE_HOME/constitution.json" },
    body: {
      en: "Structured data, not a raw prompt editor: guided setup renders it into a model-facing prose block. Drafted with your model's help, ratified by you, and carried across every project.",
      zh: "结构化数据，不是裸的提示词编辑器：引导式设置把它渲染成面向模型的 prose 区块。可由模型协助起草，经你批准生效，跨项目随身携带。",
    },
  },
  {
    n: "03",
    name: { en: "Your repo's rules", zh: "仓库自己的规则" },
    path: { en: ".codewhale/constitution.json", zh: ".codewhale/constitution.json" },
    body: {
      en: "Protected invariants, branch policy, verification requirements, escalation conditions — loaded as a repo-local authority block above project instructions, memory, and handoffs.",
      zh: "受保护的不变量、分支策略、验证要求、升级条件——作为仓库本地的权威区块加载，位阶高于项目说明、记忆与交接。",
    },
  },
];

export type Scene = {
  /** Short tab label for players that show one scene at a time. */
  tab: { en: string; zh: string };
  context: { en: string; zh: string };
  trace: string;
  cites: string[];
  decision: { en: string; zh: string };
};

export const CONSTITUTION_SCENES: Scene[] = [
  {
    tab: { en: "Stale handoff", zh: "过期交接" },
    context: {
      en: "When the prior session's summary didn't match the repo…",
      zh: "当上一会话的总结与仓库真实状态不符时……",
    },
    trace:
      "Per Article II (truth) and V (verification), I report what's actually in the tree, not what the previous agent claimed.",
    cites: ["Article II · Truth", "Article V · Verification"],
    decision: {
      en: "Reported the uncommitted work the summary left out, instead of repeating the claim.",
      zh: "如实报告了总结遗漏的未提交改动，而不是照搬旧说法。",
    },
  },
  {
    tab: { en: "Conflicting orders", zh: "指令冲突" },
    context: {
      en: "When a “pause” signal conflicted with the user's request to continue…",
      zh: "当「暂停」信号与用户要求继续相冲突时……",
    },
    trace:
      "The user's current request outranks the runtime statute, so I keep executing the task, not stopping on a technicality.",
    cites: ["Article III · User agency", "Article VII · Hierarchy"],
    decision: {
      en: "Finished the work the user actually asked for.",
      zh: "完成了用户真正要求的工作。",
    },
  },
  {
    tab: { en: "Community credit", zh: "社区署名" },
    context: {
      en: "When trimming the README, tempted to drop the contributor wall…",
      zh: "精简 README 时，本可删掉贡献者名单……",
    },
    trace:
      "Deleting community credit would break the project's own ethos. Relocate it instead. No names lost.",
    cites: ["Community ethos"],
    decision: {
      en: "Moved 200 lines of credits to docs/CONTRIBUTORS.md and linked from the README.",
      zh: "把 200 行贡献记录迁到 docs/CONTRIBUTORS.md，并在 README 中给出链接。",
    },
  },
];
