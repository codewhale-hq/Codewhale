import type { LocalizedText } from "./vocabulary";
import { pickText } from "@/lib/i18n/dictionaries";

type Rows = readonly [number, number];

export const NATIVE_TERMINAL_VIEWS = [
  {
    id: "launch",
    wide: "cols100/00-launch",
    narrow: "cols48/00-launch",
    wideRows: [5, 30] as Rows,
    label: { en: "First launch", zh: "首次启动" },
    description: {
      en: "Run codewhale in a git repository. On first launch no model is connected yet, and nothing asks you to sign in.",
      zh: "在 git 仓库中运行 codewhale。首次启动时尚未连接模型，也不会要求你登录。",
    },
  },
  {
    id: "model",
    wide: "cols100/01-model-picker",
    narrow: "cols48/01-model-picker",
    label: { en: "Model", zh: "模型" },
    description: {
      en: "The model picker, searched for deepseek-flash, lists the same model from several providers.",
      zh: "在模型选择器中搜索 deepseek-flash，会列出多个提供商提供的同一个模型。",
    },
  },
  {
    id: "edit",
    wide: "cols100/03-task-diff",
    narrow: "cols48/03-task-diff",
    label: { en: "Edit", zh: "编辑" },
    description: {
      en: "deepseek-flash read two files and edited ls.py. Accept Edits applied the edit and shows the diff.",
      zh: "deepseek-flash 读取了两个文件并编辑了 ls.py。Accept Edits 模式直接应用了修改，并显示差异。",
    },
  },
  {
    id: "approval",
    wide: "cols100/04-task-approval",
    narrow: "cols48/04-task-approval",
    label: { en: "Approval", zh: "审批" },
    description: {
      en: "The test command waits for your answer before it runs.",
      zh: "测试命令在运行前等待你的回答。",
    },
  },
  {
    id: "result",
    wide: "cols100/05-task-result",
    narrow: "cols48/05-task-result",
    label: { en: "Result", zh: "结果" },
    description: {
      en: "After you allowed it once, the tests ran and passed, and the model summarized what it changed.",
      zh: "你允许一次后，测试运行并通过，模型总结了它所做的修改。",
    },
  },
  {
    id: "receipt",
    wide: "cols56/06b-receipt-tall",
    narrow: "cols48/06b-receipt-tall",
    wideRows: [6, 37] as Rows,
    narrowRows: [2, 34] as Rows,
    label: { en: "Receipt", zh: "回执" },
    description: {
      en: "/receipts lists the file it changed, the command it ran, who approved it and the tokens it used.",
      zh: "/receipts 列出它修改的文件、运行的命令、由谁批准以及使用的 token。",
    },
  },
] as const satisfies readonly {
  id: string;
  wide: string;
  narrow: string;
  wideRows?: Rows;
  narrowRows?: Rows;
  label: LocalizedText;
  description: LocalizedText;
}[];

export const NATIVE_TERMINAL_COPY = {
  title: { en: "Explore the terminal", zh: "探索终端界面" },
  description: {
    en: "These views come from real sessions on a development build of 0.10.3, which is not released yet: deepseek-flash through DeepSeek's API, in Accept Edits.",
    zh: "这些画面来自真实会话，运行在尚未发布的 0.10.3 开发版本上：通过 DeepSeek API 使用 deepseek-flash，处于 Accept Edits 模式。",
  },
  viewsLabel: { en: "Terminal views", zh: "终端视图" },
} satisfies Record<string, LocalizedText>;

export function getNativeTerminalCopy(locale: string) {
  return {
    title: pickText(NATIVE_TERMINAL_COPY.title, locale),
    description: pickText(NATIVE_TERMINAL_COPY.description, locale),
    viewsLabel: pickText(NATIVE_TERMINAL_COPY.viewsLabel, locale),
    views: NATIVE_TERMINAL_VIEWS.map((view) => ({
      ...view,
      label: pickText(view.label, locale),
      description: pickText(view.description, locale),
    })),
  };
}
