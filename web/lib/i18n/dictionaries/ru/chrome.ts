import type { ChromeDict } from "../types";

/**
 * Russian chrome dictionary — native rewrite mirroring the current English
 * direction. Key parity with `en/chrome.ts` is enforced by
 * `npm run check:locales` and `dictionaries.test.ts`.
 *
 * Terminology follows the TUI ru locale pack (`crates/tui/locales/ru.json`):
 * the modes Plan / Work / Operate and the permission postures
 * Ask / Auto-Review / Full Access stay Latin, wrapped in Russian prose
 * ("режим Operate", "режим разрешений"). The 深 seal is the masthead's mark,
 * not prose, and is shared across locales.
 */
export const chrome: ChromeDict = {
  navDocs: "Документация",
  navStart: "Начало",
  navInstall: "Установка",
  navFaq: "Вопросы",
  navCommunity: "Сообщество",
  navContribute: "Участие",

  navFeatures: "Возможности",
  navProduct: "Продукт",
  navModels: "Модели",
  navPlugins: "Плагины",

  skipToContent: "Перейти к основному содержимому",

  navPrimaryAria: "Основная навигация",
  navHomeAria: "Главная Codewhale",

  installCta: "Установить →",

  dateLocale: "ru-RU",

  menuOpen: "Открыть меню",
  menuClose: "Закрыть меню",

  themeAuto: "авто",
  themeLight: "светлая",
  themeDark: "тёмная",
  themeAria: "Тема: {mode} (нажмите, чтобы переключить)",
  themeTitle: "Тема · авто / светлая / тёмная",

  footerTagline: "Используйте любую модель через агента, код которого можно прочитать.",
  footerProduct: "Продукт",
  footerProject: "Проект",
  footerDocs: "Документация",
  footerGuide: "С чего начать",
  footerInstall: "Установка",
  footerApps: "Десктоп и веб",
  footerModels: "Модели",
  footerRuntime: "Рантайм",
  footerFaq: "Вопросы и ответы",
  footerIssues: "Задачи",
  footerContribute: "Участие",
  footerRoadmap: "Дорожная карта",
  footerLicense: "Лицензия MIT",
  footerTerms: "Условия использования",
  footerPrivacy: "Конфиденциальность",
  footerChangelog: "Журнал изменений",
  footerCanonicalSource: "Канонический источник: ",
  footerReleases: " · Релизы: ",
  footerReleasesLink: "Релизы на GitHub",
  footerSecurity: "Безопасность",
  footerLegal: "Правовая информация",

  switcherLabel: "Язык",
  switcherSwitchTo: "Переключиться на {label}",
  partialBadge: "(частично)",
};
