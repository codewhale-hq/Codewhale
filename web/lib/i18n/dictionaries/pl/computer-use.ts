import type { ComputerUseDict } from "../types";

/**
 * Polish dictionary for `app/[locale]/computer-use/page.tsx` and the
 * Computer Use section of the install page. Product names, the menu items
 * Pause, Stop and Check for updates, and the macOS setting names stay as
 * the app shows them.
 */
export const computerUse: ComputerUseDict = {
  metaTitle: "Computer Use · Wersja poglądowa · Codewhale",
  metaDescription: "Pobierz i skonfiguruj Codewhale Computer Use na Maca. Sterowanie aplikacjami w tle, konfiguracja uprawnień oraz przyciski Pause i Stop pod Twoją kontrolą.",
  title: "Computer Use",
  lead: "Pozwól swojemu agentowi pracować w aplikacjach na Macu, gdy nadal korzystasz z komputera.",
  publisher: "Od Codewhale",
  download: "Pobierz na Maca",
  downloadZip: "Archiwum ZIP (używane przez aktualizator w aplikacji)",
  requirements: "macOS 13.5 lub nowszy · Apple silicon i Intel",
  included: "Jedna aplikacja do pobrania. Nie trzeba osobno instalować Node ani kompilatora.",
  pendingTitle: "Wersja na Maca w przygotowaniu",
  pendingBody: "Publiczny instalator pojawi się tutaj po zakończeniu notaryzacji Apple i kontroli wydania.",
  unavailableTitle: "Nie udało się sprawdzić dostępności pobierania",
  unavailableBody: "Odśwież stronę, aby spróbować ponownie, albo sprawdź opublikowane wydania poniżej.",
  releases: "Opublikowane wydania",
  receipt: "Dane do weryfikacji pobranego pliku",
  setup: "Skonfiguruj Maca",
  steps: [
    { title: "Zainstaluj aplikację", body: "Otwórz obraz dysku i przeciągnij Codewhale Computer Use do folderu „Programy”. Uruchom aplikację z folderu „Programy”, a następnie wybierz Computer Use z menu pod ikoną wieloryba w pasku menu." },
    { title: "Sprawdź uprawnienia", body: "Przyciski konfiguracji otwierają sekcje „Dostępność” i „Nagrywanie ekranu” w Ustawieniach systemowych. To Ty decydujesz, które uprawnienia przyznać." },
    { title: "Połącz swojego agenta", body: "Wybierz poniżej konfigurację Codewhale dla swojej wersji CLI. Dla innych zgodnych klientów MCP skorzystaj z instrukcji konfiguracji w repozytorium." },
  ],
  controlsTitle: "Zachowaj kontrolę",
  controlsBody: "Po przyznaniu uprawnień macOS tryb standardowy działa bez pytania o zgodę dla każdej aplikacji. Zablokowane przez Ciebie aplikacje pozostają niedostępne; Pause wstrzymuje korzystanie z komputera, a Stop computer use kończy bieżące sesje. Ścisłe zatwierdzanie jest opcjonalne. Poproś o podgląd na żywo, gdy chcesz śledzić pracę.",
  updateTitle: "Aktualizacje wtedy, gdy chcesz",
  updateBody: "Wybierz w aplikacji Check for updates (sprawdź aktualizacje). Przed instalacją aktualizacji pomocnik sprawdza pobrany plik, podpis Codewhale i notaryzację Apple, a poprzednią wersję zachowuje na wypadek konieczności przywrócenia.",
  help: "Konfiguracja i rozwiązywanie problemów",
  notes: "Informacje o wydaniu",
  demo: "Zobacz test w tle",
  source: "Zobacz na GitHubie",
  platforms: "Pomocnik na Maca jest w wersji beta, a wtyczka z katalogu obsługuje tylko Maca. Opublikowane wydania zawierają też niepodpisaną eksperymentalną wersję podglądową dla Windows x64; Linux wymaga eksperymentalnej konfiguracji ze źródeł lub przez Docker. Wprowadzanie danych w Windows i Linuksie korzysta ze wspólnego pulpitu.",
  connectTitle: "Połącz Codewhale",
  connectVersion: "Codewhale 0.10.2 zawiera starszą wtyczkę Computer Use. Instalacja pozycji z katalogu jej nie aktualizuje. Połącz zainstalowaną aplikację pomocniczą Mac bezpośrednio przez MCP.",
  connectLimit: "Bezpośrednie połączenie MCP dodaje narzędzia bez umiejętności wtyczki. Nie używaj go równocześnie ze starszą wbudowaną wtyczką. Jeśli jest włączona, najpierw uruchom /plugin disable computer-use.",
  connectRuntime: "Aplikacja zawiera Node; osobna instalacja nie jest potrzebna. Jeśli jest zainstalowana gdzie indziej, ustaw APP na rzeczywistą ścieżkę pakietu aplikacji.",
  connectReload: "W działającej sesji terminala uruchom /mcp reload i poproś:",
  connectPrompt: "Sprawdź uprawnienia Computer Use za pomocą request_access. Nie wysyłaj żadnych danych wejściowych.",
  connectExpected: "Sprawdź w odpowiedzi via: app, wersję aplikacji pomocniczej i brakujące uprawnienia. Jeśli aplikacji pomocniczej nie ma, zatrzymaj się i postępuj zgodnie ze wskazówką konfiguracji. To sprawdza tylko połączenie i uprawnienia.",
  connectGuide: "Instrukcja konfiguracji MCP",
  contributeTitle: "Pomóż ulepszyć Computer Use",
  contributeBody: "Popraw dokumentację lub przetestuj przebieg pracy w aplikacji i podziel się powtarzalnymi wynikami.",
  contributeGuide: "Przewodnik dla współtwórców",
  contributeIssue: "Zgłoś problem",
};
