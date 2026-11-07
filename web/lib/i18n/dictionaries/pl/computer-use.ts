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
    { title: "Połącz swojego agenta", body: "W Codewhale zainstaluj Computer Use poleceniem /plugin marketplace install codewhale computer-use, a następnie przejrzyj i włącz wtyczkę. Pomocnik jest instalowany osobno. Inne zgodne hosty MCP łączą się z nim zgodnie z instrukcją konfiguracji." },
  ],
  controlsTitle: "Zachowaj kontrolę",
  controlsBody: "Po przyznaniu uprawnień macOS tryb standardowy działa bez pytania o zgodę dla każdej aplikacji. Zablokowane przez Ciebie aplikacje pozostają niedostępne; Pause wstrzymuje korzystanie z komputera, a Stop computer use kończy bieżące sesje. Ścisłe zatwierdzanie jest opcjonalne. Poproś o podgląd na żywo, gdy chcesz śledzić pracę.",
  updateTitle: "Aktualizacje wtedy, gdy chcesz",
  updateBody: "Wybierz w aplikacji Check for updates (sprawdź aktualizacje). Przed instalacją aktualizacji pomocnik sprawdza pobrany plik, podpis Codewhale i notaryzację Apple, a poprzednią wersję zachowuje na wypadek konieczności przywrócenia.",
  help: "Konfiguracja i rozwiązywanie problemów",
  notes: "Informacje o wydaniu",
  demo: "Zobacz test w tle",
  source: "Kod źródłowy i inne platformy",
  platforms: "Pomocnik na Maca jest w wersji beta, a wtyczka z katalogu obsługuje tylko Maca. Opublikowane wydania zawierają też niepodpisaną eksperymentalną wersję podglądową dla Windows x64; Linux wymaga eksperymentalnej konfiguracji ze źródeł lub przez Docker. Wprowadzanie danych w Windows i Linuksie korzysta ze wspólnego pulpitu.",
};
