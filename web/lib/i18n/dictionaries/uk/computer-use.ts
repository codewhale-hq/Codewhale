import type { ComputerUseDict } from "../types";

/**
 * Ukrainian dictionary for `app/[locale]/computer-use/page.tsx` and the
 * Computer Use section of the install page. Product names (Codewhale,
 * Computer Use), the menu items Pause, Stop and Check for updates stay as
 * the app shows them; the macOS setting and folder names use the Ukrainian
 * system UI names (Доступність, Запис екрана, Програми).
 */
export const computerUse: ComputerUseDict = {
  metaTitle: "Computer Use · Попередня версія · Codewhale",
  metaDescription: "Завантажте й налаштуйте Codewhale Computer Use для Mac. Фонове керування застосунками, налаштування дозволів і кнопки Pause та Stop під вашим контролем.",
  title: "Computer Use",
  lead: "Нехай Codewhale працює у ваших застосунках, поки ви продовжуєте свою роботу. Надайте дозвіл один раз на цьому Mac і зупиняйте використання комп’ютера, коли забажаєте.",
  publisher: "Від Codewhale",
  download: "Завантажити для Mac",
  downloadZip: "Архів ZIP (використовується вбудованим оновлювачем)",
  requirements: "macOS 13.5 або новіша · Apple silicon та Intel",
  included: "Один застосунок для завантаження. Окремо встановлювати Node чи компілятор не потрібно.",
  pendingTitle: "Завантаження для Mac готується",
  pendingBody: "Публічний інсталятор з’явиться тут після нотаризації Apple та перевірок перед випуском.",
  unavailableTitle: "Не вдалося перевірити доступність завантаження",
  unavailableBody: "Оновіть сторінку, щоб спробувати ще раз, або перегляньте опубліковані релізи нижче.",
  releases: "Опубліковані релізи",
  receipt: "Дані для перевірки завантаження",
  setup: "Налаштуйте свій Mac",
  steps: [
    { title: "Установіть застосунок", body: "Відкрийте образ диска й перетягніть Codewhale Computer Use у папку «Програми». Відкрийте його з папки «Програми», а потім виберіть Computer Use у меню з іконкою кита в смузі меню." },
    { title: "Перегляньте дозволи", body: "За допомогою кнопок налаштування відкрийте розділи «Доступність» і «Запис екрана» в Системних параметрах. Які дозволи надати — вирішуєте ви." },
    { title: "Увімкніть Computer Use", body: "У налаштуваннях Connectors у Codewhale встановіть або оновіть Computer Use, потім перегляньте й увімкніть плагін. Попросіть його попрацювати в застосунку. Під час першого використання виберіть Trust until revoked у нативному помічнику; довіра зберігається при зміні застосунків, моделей і завдань." },
  ],
  controlsTitle: "Контроль залишається у вас",
  controlsBody: "Компактний елемент керування Codewhale з’являється поруч із застосунком, у якому він працює. Pause призупиняє використання комп’ютера, а Stop computer use завершує всі поточні сеанси роботи з комп’ютером. Виберіть Turn off у помічнику, щоб відкликати доступ Codewhale. Якщо хочете стежити за роботою, попросіть увімкнути попередній перегляд наживо.",
  updateTitle: "Оновлення — коли вирішите ви",
  updateBody: "Виберіть у застосунку Check for updates (перевірити оновлення). Перед встановленням оновлення він перевіряє завантажений файл, підпис Codewhale і нотаризацію Apple, а попередню версію застосунку зберігає для відновлення.",
  help: "Налаштування й усунення несправностей",
  notes: "Примітки до випуску",
  demo: "Подивитися фонову перевірку",
  source: "Вихідний код та інші платформи",
  platforms: "Це завантаження для Mac. На Windows і Linux наразі використовуються плагін із вихідного коду та налаштування на боці хоста.",
};
