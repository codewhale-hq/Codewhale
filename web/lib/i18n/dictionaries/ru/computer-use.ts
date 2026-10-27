import type { ComputerUseDict } from "../types";

/**
 * Russian dictionary for `app/[locale]/computer-use/page.tsx` and the
 * Computer Use section of the install page. Product names, the menu items
 * Pause, Stop and Check for updates, and the macOS setting names stay as the
 * app shows them; the macOS setting and folder names use the Russian system
 * UI names («Универсальный доступ», «Запись экрана», «Программы»).
 */
export const computerUse: ComputerUseDict = {
  metaTitle: "Computer Use · Предварительная версия · Codewhale",
  metaDescription: "Скачайте и настройте Codewhale Computer Use для Mac: фоновое управление приложениями, настройка разрешений и кнопки Pause и Stop под вашим контролем.",
  title: "Computer Use",
  lead: "Пусть Codewhale работает в ваших приложениях, пока вы продолжаете свою работу. Разрешите доступ один раз на этом Mac и останавливайте работу с компьютером, когда захотите.",
  publisher: "От Codewhale",
  download: "Скачать для Mac",
  downloadZip: "ZIP-архив (используется встроенным обновлением)",
  requirements: "macOS 13.5 или новее · Apple silicon и Intel",
  included: "Скачивается одно приложение. Отдельно устанавливать Node или компилятор не нужно.",
  pendingTitle: "Загрузка для Mac готовится",
  pendingBody: "Публичный установщик появится здесь после нотаризации Apple и проверок перед выпуском.",
  unavailableTitle: "Не удалось проверить доступность загрузки",
  unavailableBody: "Обновите страницу, чтобы попробовать снова, или посмотрите опубликованные релизы ниже.",
  releases: "Опубликованные релизы",
  receipt: "Сведения для проверки загрузки",
  setup: "Настройте свой Mac",
  steps: [
    { title: "Установите приложение", body: "Откройте образ диска и перетащите Codewhale Computer Use в папку «Программы». Запустите его из папки «Программы», затем нажмите значок кита в строке меню и выберите Computer Use." },
    { title: "Проверьте разрешения", body: "Кнопки настройки открывают разделы «Универсальный доступ» и «Запись экрана» в Системных настройках. Какие разрешения выдать — решаете вы." },
    { title: "Включите Computer Use", body: "В настройках Connectors в Codewhale установите или обновите Computer Use, затем проверьте и включите плагин. Попросите его поработать в приложении. При первом использовании выберите Trust until revoked в нативном помощнике; доверие сохраняется при смене приложений, моделей и задач." },
  ],
  controlsTitle: "Контроль остаётся у вас",
  controlsBody: "Компактный элемент управления Codewhale появляется рядом с используемым приложением. Pause приостанавливает работу с компьютером, а Stop computer use завершает все текущие сеансы работы с компьютером. Выберите Turn off в помощнике, чтобы отозвать доступ Codewhale. Если хотите наблюдать за работой, попросите включить предпросмотр в реальном времени.",
  updateTitle: "Обновления — когда решите вы",
  updateBody: "Выберите в приложении Check for updates (проверить обновления). Перед установкой обновления помощник проверяет загруженный файл, подпись Codewhale и нотаризацию Apple, а предыдущую версию сохраняет на случай восстановления.",
  help: "Настройка и устранение неполадок",
  notes: "Примечания к выпуску",
  demo: "Посмотреть фоновую проверку",
  source: "Исходный код и другие платформы",
  platforms: "Эта загрузка предназначена для Mac. На Windows и Linux пока используются плагин из исходного кода и настройка на стороне хоста.",
};
