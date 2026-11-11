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
  navContribute: "Участие",
  navSignIn: "Войти",
  navDesktop: "Десктоп",

  navFeatures: "Возможности",
  navModels: "Модели",
  navPlugins: "Плагины",

  skipToContent: "Перейти к основному содержимому",

  navPrimaryAria: "Основная навигация",
  navHomeAria: "Главная Codewhale",

  installCta: "Установить →",

  dateLocale: "ru-RU",

  menuOpen: "Открыть меню",
  menuClose: "Закрыть меню",
  translationMachineDraft: "Этот перевод — машинный черновик, ещё не проверенный носителями языка.",

  themeAuto: "авто",
  themeLight: "светлая",
  themeDark: "тёмная",
  themeAria: "Тема: {mode} (нажмите, чтобы переключить)",
  themeTitle: "Тема · авто / светлая / тёмная",

  footerProduct: "Продукт",
  footerProject: "Проект",
  footerDocs: "Документация",
  footerGuide: "С чего начать",
  footerInstall: "Установка",
  footerModels: "Модели",
  footerFaq: "Вопросы и ответы",
  footerContribute: "Участие",
  footerRoadmap: "Дорожная карта",
  footerLicense: "Лицензия MIT",
  footerMerch: "Мерч",
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

  stateReleased: "Выпущено",
  stateAvailable: "Доступно",
  stateBeta: "Бета",
  stateDev: "Сборка для разработки",
  stateOpen: "Открыто",
  stateInviteOnly: "Только по приглашениям",
  stateNotOpen: "Пока не открыто",
  stateNotPublic: "Не опубликовано",
  stateInSource: "Есть в исходном коде, ещё не выпущено",
  numbersAsOf: "По состоянию на {date}",
  downloadMacos: "Скачать для macOS",
  downloadVersion: "Версия {version}",
  downloadSize: "Размер {size}",
  downloadChecksum: "Контрольная сумма SHA-256",
  downloadSigned: "Подписано и нотаризовано",
};
