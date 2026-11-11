import type { ContributeDict } from "../types";

/** Japanese dictionary for `app/[locale]/contribute/page.tsx`. */
export const contribute: ContributeDict = {
  metaTitle: "貢献する · Codewhale",
  metaDescription:
    "コードの有無にかかわらず、すべてのフィードバックが Codewhale の助けになります。バグ、アイデア、翻訳、プロバイダー、修正をどこに送ればよいかが分かります。",
  kicker: "Codewhale を一緒に作る",
  title: "Codewhale づくりに参加する",
  lede: "コードがなくても、すべてのフィードバックが役に立ちます。バグ、アイデア、おかしく見えた画面、どれも始めるきっかけとして十分です。",
  fileIssue: "issue を作成",
  browsePulls: "pull requests を見る",
  fullGuide: "貢献ガイド",
  pathsTitle: "最初の一歩を選ぶ",
  workflowTitle: "他の人がレビューしやすい変更を送る",
  reviewTitle: "何を変え、どう確認したかを示す",
  reviewScope:
    "問題、変更の理由、テストの根拠、残るリスクをレビュアーに伝えてください。範囲を絞るほど、フィードバックも明確になります。",
  devTitle: "コードの変更をビルドして確認する",
  devScope:
    "Rust のコードを変更する場合は、関連部分をビルドし、その部分に絞ったチェックを実行してください。変更内容に応じて必要になる広範なチェックは、貢献ガイドに従ってください。",
  mergedAccountsCount: {
    other: "過去 30 日間に {n} 件の GitHub アカウントの pull request がマージされました。",
  },
  mergedPeopleCount: {
    other: "過去 30 日間に {n} 人の pull request がマージされました。",
  },
};
