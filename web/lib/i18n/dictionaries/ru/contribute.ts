import type { ContributeDict } from "../types";

/** Russian dictionary for `app/[locale]/contribute/page.tsx`. */
export const contribute: ContributeDict = {
  metaTitle: "Участие · Codewhale",
  metaDescription:
    "Любая обратная связь помогает Codewhale, с кодом или без него. Узнайте, куда отправить ошибку, идею, перевод, провайдера или исправление.",
  kicker: "Создаём Codewhale вместе",
  title: "Помогите создавать Codewhale",
  lede: "Полезна любая обратная связь, с кодом или без него. Чтобы начать, достаточно ошибки, идеи или экрана, который выглядел неправильно.",
  fileIssue: "Создать issue",
  browsePulls: "Открытые pull request",
  fullGuide: "Руководство для участников",
  pathsTitle: "Выберите, с чего начать",
  workflowTitle: "Отправьте изменение, которое удобно проверять",
  reviewTitle: "Покажите, что изменилось и как вы это проверили",
  reviewScope:
    "Расскажите рецензентам о проблеме, причине изменения, результатах тестов и оставшихся рисках. Чем уже область изменения, тем яснее отзывы.",
  devTitle: "Соберите и проверьте изменения в коде",
  devScope:
    "Для кода на Rust соберите нужную часть и запустите точечные проверки. Более широкие проверки, которые нужны вашему изменению, описаны в руководстве для участников.",
  mergedAccountsCount: {
    one: "За последние 30 дней приняты pull request от {n} аккаунта GitHub.",
    few: "За последние 30 дней приняты pull request от {n} аккаунтов GitHub.",
    many: "За последние 30 дней приняты pull request от {n} аккаунтов GitHub.",
    other: "За последние 30 дней приняты pull request от {n} аккаунта GitHub.",
  },
  mergedPeopleCount: {
    one: "За последние 30 дней приняты pull request от {n} человека.",
    few: "За последние 30 дней приняты pull request от {n} человек.",
    many: "За последние 30 дней приняты pull request от {n} человек.",
    other: "За последние 30 дней приняты pull request от {n} человека.",
  },
};
