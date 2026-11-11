import type { ContributeDict } from "../types";

/** Ukrainian dictionary for `app/[locale]/contribute/page.tsx`. */
export const contribute: ContributeDict = {
  metaTitle: "Долучитися · Codewhale",
  metaDescription:
    "Будь-який відгук допомагає Codewhale, із кодом чи без нього. Дізнайтеся, куди надсилати помилку, ідею, переклад, постачальника чи виправлення.",
  kicker: "Створюймо Codewhale разом",
  title: "Допоможіть створювати Codewhale",
  lede: "Допомагає будь-який відгук, із кодом чи без нього, а щоб почати, досить помилки, ідеї або екрана, що виглядав неправильно.",
  fileIssue: "Відкрити issue",
  browsePulls: "Переглянути pull request",
  fullGuide: "Посібник для учасників",
  pathsTitle: "Оберіть, з чого почати",
  workflowTitle: "Надішліть зміну, яку можуть переглянути інші",
  reviewTitle: "Покажіть, що змінилося і як ви це перевірили",
  reviewScope:
    "Опишіть рецензентам проблему, причину зміни, докази тестування та залишкові ризики. Вузький обсяг дає чіткіші відгуки.",
  devTitle: "Зберіть і перевірте свою зміну в коді",
  devScope:
    "Для коду на Rust зберіть відповідну частину коду та запустіть її цільові перевірки. Для ширших перевірок, потрібних вашій зміні, дотримуйтеся посібника для учасників.",
  mergedAccountsCount: {
    one: "За останні 30 днів прийнято pull request від {n} акаунта GitHub.",
    few: "За останні 30 днів прийнято pull request від {n} акаунтів GitHub.",
    many: "За останні 30 днів прийнято pull request від {n} акаунтів GitHub.",
    other: "За останні 30 днів прийнято pull request від {n} акаунта GitHub.",
  },
  mergedPeopleCount: {
    one: "За останні 30 днів прийнято pull request від {n} людини.",
    few: "За останні 30 днів прийнято pull request від {n} людей.",
    many: "За останні 30 днів прийнято pull request від {n} людей.",
    other: "За останні 30 днів прийнято pull request від {n} людини.",
  },
};
