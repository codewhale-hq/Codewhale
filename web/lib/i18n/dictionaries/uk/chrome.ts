import type { ChromeDict } from "../types";

/**
 * Ukrainian chrome pack — native rewrite mirroring the current English copy;
 * the old "local-first" tag is gone ("any model, on your machine" instead).
 * Terminology follows the TUI locale pack (`crates/tui/locales/uk.json`):
 * режим дозволів for the permission posture, провайдер, репозиторій,
 * композер, міркування. Plan / Work / Operate and Ask / Auto-Review /
 * Full Access stay literal there and stay literal here.
 */
export const chrome: ChromeDict = {
  navDocs: "Документація",
  navContribute: "Участь",
  navSignIn: "Увійти",
  navDesktop: "Десктоп",

  navFeatures: "Можливості",
  navModels: "Моделі",
  navPlugins: "Плагіни",

  skipToContent: "Перейти до основного вмісту",

  navPrimaryAria: "Основна навігація",
  navHomeAria: "Головна сторінка Codewhale",

  installCta: "Встановити →",

  dateLocale: "uk-UA",

  menuOpen: "Відкрити меню",
  menuClose: "Закрити меню",
  translationMachineDraft: "Цей переклад є машинною чернеткою, яку ще не перевіряли носії мови.",

  themeAuto: "авто",
  themeLight: "світла",
  themeDark: "темна",
  themeAria: "Тема: {mode} (натисніть, щоб перемкнути)",
  themeTitle: "Тема · авто / світла / темна",

  footerProduct: "Продукт",
  footerProject: "Проєкт",
  footerDocs: "Документація",
  footerGuide: "З чого почати",
  footerInstall: "Встановлення",
  footerModels: "Моделі",
  footerFaq: "Питання та відповіді",
  footerContribute: "Участь",
  footerRoadmap: "Дорожня карта",
  footerLicense: "Ліцензія MIT",
  footerMerch: "Мерч",
  footerTerms: "Умови використання",
  footerPrivacy: "Приватність",
  footerChangelog: "Журнал змін",
  footerCanonicalSource: "Канонічне джерело: ",
  footerReleases: " · Релізи: ",
  footerReleasesLink: "Релізи на GitHub",
  footerSecurity: "Безпека",
  footerLegal: "Правова інформація",

  switcherLabel: "Мова",
  switcherSwitchTo: "Перемкнути на {label}",
  partialBadge: "(частково)",

  stateReleased: "Випущено",
  stateAvailable: "Доступно",
  stateBeta: "Бета",
  stateDev: "Збірка для розробки",
  stateOpen: "Відкрито",
  stateInviteOnly: "Лише за запрошеннями",
  stateNotOpen: "Ще не відкрито",
  stateNotPublic: "Не опубліковано",
  stateInSource: "Є у вихідному коді, ще не випущено",
  numbersAsOf: "Станом на {date}",
  downloadMacos: "Завантажити для macOS",
  downloadVersion: "Версія {version}",
  downloadSize: "Розмір {size}",
  downloadChecksum: "Контрольна сума SHA-256",
  downloadSigned: "Підписано й нотаризовано",
};
