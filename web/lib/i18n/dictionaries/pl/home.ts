import type { HomeDict } from "../types";

/** Polish home copy: useful work with chosen models and connected tools. */

export const home: HomeDict = {
  metaTitle: "Codewhale: otwarty agent programistyczny",
  metaDescription: "Codewhale to agent programistyczny z otwartym kodem dla terminala. Czytaj, edytuj i testuj projekty z wybranymi modelami zdalnymi lub lokalnymi.",
  heroTitle: "Agent programistyczny z otwartym kodem dla każdego modelu",
  heroIntro: "{brand} czyta Twój projekt, edytuje pliki i uruchamia testy z terminala. Połącz model hostowany lub lokalny i wybierz działania wymagające Twojego zatwierdzenia.",
  getCodewhale: "Zainstaluj Codewhale",
  heroInstallAria: "Polecenie instalacji",
  exploreProduct: "Poznaj Codewhale",
  shotPreview: "Podgląd terminala",
  screenshotAlt:
    "Codewhale v{version}: zrzut terminala z rozmową, polem wiadomości i kontrolkami sesji.",
  latestRelease: "Najnowsze wydanie {tag}",
  releaseUnavailable: "Status wydania niedostępny",
  proofContributors: "Współtwórcy: {count}",
  currentSource: "Źródło",
  sourceCandidate: "Niewydane",
  publishedRelease: "wydane",
  gainHeading: "Twórz i automatyzuj z terminala",
  gainLede:
    "Opisz, co chcesz stworzyć lub zautomatyzować. Codewhale może edytować pliki, uruchamiać polecenia i sprawdzać wyniki, a Ty kontrolujesz jego dostęp.",
  gain: [
    [
      "Twórz aplikacje i narzędzia",
      "Stwórz aplikację, dodaj funkcję lub napisz skrypt. Codewhale może pracować z plikami projektu, uruchamiać kod i testować to, co tworzy."
    ],
    [
      "Automatyzuj powtarzalną pracę",
      "Uruchamiaj przepływy pracy z terminala, skryptów lub CI. Przy większych zadaniach deleguj części pracy zespołowi Fleet złożonemu z agentów korzystających z różnych modeli."
    ],
    [
      "Połącz swoje narzędzia",
      "Dodawaj narzędzia przez wtyczki i serwery MCP lub korzystaj z API we własnych skryptach. Każda usługa wymaga osobnej konfiguracji i uwierzytelnienia."
    ]
  ],
  chapterModels: "Twoje modele",
  modelsHeading: "Używaj wybranych przez siebie modeli",
  modelsBody:
    "Połącz konta dostawców, endpoint zgodny z OpenAI albo modele lokalne i utrzymywane na własnej infrastrukturze. Wybierz model dla sesji i dla każdego agenta w zespole Fleet.",
  modelsFacts: [
    [
      "Twoje konta API",
      "Połącz dostawców takich jak DeepSeek, OpenAI, Anthropic lub Google za pomocą własnych kluczy. Domyślny model deepseek-flash korzysta teraz z DeepSeek-V4.1-Flash."
    ],
    [
      "Twoja brama",
      "Użyj punktu końcowego zgodnego z OpenAI i wybierz oferowane przez niego modele."
    ],
    [
      "Twoje wnioskowanie",
      "Uruchamiaj modele lokalne lub samodzielnie hostowane przez Ollama, vLLM lub SGLang."
    ],
    [
      "Twój plan ChatGPT",
      "Zaloguj się przez codewhale auth chatgpt, aby korzystać z planu ChatGPT zamiast klucza API. Funkcja eksperymentalna."
    ]
  ],
  modelsLink: "Przeglądaj modele i dostawców",
  startHeading: "Jak zacząć",
  startLede:
    "Zainstaluj Codewhale, połącz model i otwórz folder projektu. W razie potrzeby możesz dodać wtyczki i kolejnych agentów.",
  startGuideLink: "Skorzystaj z przewodnika na start",
  startVocabularyLink: "Zobacz słownik produktu",
  chapterAvailability: "Gdzie działa",
  availabilityHeading: "Dostępne teraz i w fazie rozwoju",
  availabilityLede:
    "Terminal i lokalny klient przeglądarkowy są dostępne. Natywna aplikacja desktopowa i przebudowana aplikacja webowa są w trakcie rozwoju.",
  availability: [
    [
      "Terminal i lokalna przeglądarka",
      "Wydany",
      "Zainstaluj w systemie Linux, macOS lub Windows, a następnie uruchom codewhale albo codewhale web, aby otworzyć lokalnego klienta w przeglądarce. Działają też npm i Cargo; Android w Termux to wersja podglądowa."
    ],
    [
      "CodeWhale GUI (VS Code)",
      "Dostępny",
      "Osobny projekt utrzymywany przez społeczność: czat, wątki i zmiany plików w panelu bocznym VS Code na tym samym Codewhale Runtime. Zainstaluj z VS Code Marketplace.",
      "https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode"
    ],
    [
      "Hostowana aplikacja webowa",
      "W rozwoju",
      "Budowana od nowa według wzoru aplikacji desktopowej i jeszcze niedostępna. Aby już dziś korzystać z widoku w przeglądarce, uruchom codewhale web na swoim komputerze."
    ],
    [
      "Aplikacja desktopowa",
      "Kompilacja deweloperska",
      "Natywna aplikacja na tym samym silniku. Przeglądaj i zatwierdzaj zmiany, śledź pracę w wielu projektach i edytuj wiadomość, aby wysłać ją ponownie jako nową gałąź. Publiczna wersja do pobrania nie jest jeszcze dostępna."
    ]
  ],
  availabilityNote:
    "Terminal, lokalna przeglądarka i GUI nie wymagają konta Codewhale. Jeśli używasz własnego klucza dostawcy, opłaty za to użycie nalicza ten dostawca.",
  surfacesHeading: "Pracuj z plikami i narzędziami",
  surfaces: [
    [
      "Pliki i terminal",
      "Twórz pliki, uruchamiaj polecenia, analizuj dane i testuj rezultaty. Ty określasz katalog roboczy i uprawnienia."
    ],
    [
      "Wtyczki i połączone aplikacje",
      "Dodawaj umiejętności i narzędzia przez wtyczki i MCP. Sprawdzaj i włączaj połączenia, z których ma korzystać agent."
    ],
    [
      "Przeglądarka i komputer · podgląd",
      "Używaj narzędzi przeglądarkowych i wtyczki Computer Use w aplikacjach i witrynach w granicach przyznanego dostępu."
    ],
    [
      "Sesje, do których wrócisz",
      "Zachowuj razem rozmowę, wyniki narzędzi i historię pracy. Wznawiaj zadanie w terminalu lub lokalnym kliencie przeglądarkowym."
    ],
    [
      "Zespoły agentów",
      "Podziel duże zadanie w Fleet między agentów z różnymi rolami i modelami. Śledź ich postępy w jednym miejscu."
    ]
  ],
  runtimeLink: "Poznaj narzędzia i integracje",
  installBandHeading: "Zainstaluj w systemie macOS lub Linux",
  copy: "Kopiuj",
  copied: "Skopiowano ✓",
  binaries: "Binarki",
  chinaMirrors: "Mirrory w Chinach",
  installGuideLink: "Przeczytaj przewodnik instalacji",
  communityHeading: "Współtwórz Codewhale",
  communityBody:
    "W tworzeniu Codewhale uczestniczyło {count} osób. Zgłoś błąd, popraw dokumentację lub dodaj kod na GitHubie.",
  communityLinksAria: "Linki społeczności",
  contribute: "Współtwórz na GitHubie",
};
