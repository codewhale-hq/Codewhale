import type { HomeDict } from "../types";

/** Italian home copy: useful work with chosen models and connected tools. */

export const home: HomeDict = {
  metaTitle: "Codewhale: agente di programmazione open source",
  metaDescription: "Codewhale è un agente di programmazione open source per il terminale. Leggi, modifica e testa progetti con i tuoi modelli remoti o locali.",
  heroTitle: "L’agente di programmazione open source per qualsiasi modello",
  heroIntro: "{brand} legge il tuo progetto, modifica i file ed esegue i tuoi test dal terminale. Collega un modello ospitato o locale e scegli quali azioni richiedono la tua approvazione.",
  getCodewhale: "Installa Codewhale",
  heroInstallAria: "Comando di installazione",
  exploreProduct: "Esplora Codewhale",
  shotPreview: "Anteprima del terminale",
  screenshotAlt:
    "Codewhale v{version}: una cattura del terminale con conversazione, campo del messaggio e controlli della sessione.",
  latestRelease: "Ultima release {tag}",
  releaseUnavailable: "Stato delle release non disponibile",
  proofContributors: "{count} contributori",
  currentSource: "Sorgente",
  sourceCandidate: "Non rilasciata",
  publishedRelease: "rilasciata",
  gainHeading: "Crea e automatizza dal tuo terminale",
  gainLede:
    "Descrivi cosa vuoi creare o automatizzare. Codewhale può modificare file, eseguire comandi e verificare il risultato, con un accesso che controlli tu.",
  gain: [
    [
      "Crea app e strumenti",
      "Crea un’app, aggiungi una funzionalità o scrivi uno script. Codewhale può lavorare sui file del progetto, eseguire il codice e testare ciò che crea."
    ],
    [
      "Automatizza il lavoro ripetitivo",
      "Esegui flussi di lavoro dal terminale, da script o dalla CI. Per le attività più grandi, delega parte del lavoro a una Fleet di agenti con modelli diversi."
    ],
    [
      "Collega gli strumenti che usi",
      "Aggiungi strumenti tramite plugin e server MCP, oppure usa le API dai tuoi script. Ogni servizio richiede una configurazione e un’autenticazione proprie."
    ]
  ],
  chapterModels: "I tuoi modelli",
  modelsHeading: "Usa i modelli che preferisci",
  modelsBody:
    "Collega i tuoi account dei provider, un endpoint compatibile con OpenAI oppure modelli locali e su infrastruttura gestita da te. Scegli un modello per la sessione e per ogni agente di una Fleet.",
  modelsFacts: [
    [
      "I tuoi account API",
      "Collega provider come DeepSeek, OpenAI, Anthropic o Google con le tue chiavi. Il modello predefinito, deepseek-flash, ora esegue DeepSeek-V4.1-Flash."
    ],
    [
      "Il tuo gateway",
      "Usa un endpoint compatibile con OpenAI e scegli i modelli che offre."
    ],
    [
      "La tua inferenza",
      "Esegui modelli locali o autogestiti con Ollama, vLLM o SGLang."
    ],
    [
      "Il tuo piano ChatGPT",
      "Accedi con codewhale auth chatgpt per usare un piano ChatGPT al posto di una chiave API. Sperimentale."
    ]
  ],
  modelsLink: "Sfoglia modelli e provider",
  startHeading: "Per iniziare",
  startLede:
    "Installa Codewhale, collega un modello e apri una cartella di progetto. Puoi aggiungere plugin e altri agenti quando ti servono.",
  startGuideLink: "Segui la guida introduttiva",
  startVocabularyLink: "Vedi il vocabolario del prodotto",
  chapterAvailability: "Dove funziona",
  availabilityHeading: "Disponibile ora e in sviluppo",
  availabilityLede:
    "Il terminale e il client browser locale sono disponibili. L’app desktop nativa e la nuova app web ospitata sono in sviluppo.",
  availability: [
    [
      "Terminale e browser locale",
      "Rilasciato",
      "Installa su Linux, macOS o Windows, poi esegui codewhale, oppure codewhale web per il client browser locale. Funzionano anche npm e Cargo; Android su Termux è in anteprima."
    ],
    [
      "CodeWhale GUI (VS Code)",
      "Disponibile",
      "Un progetto separato mantenuto dalla comunità: chat, thread e modifiche ai file in una barra laterale di VS Code sullo stesso Codewhale Runtime. Installala dal VS Code Marketplace.",
      "https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode"
    ],
    [
      "App web ospitata",
      "In sviluppo",
      "In fase di ricostruzione per corrispondere all’app desktop e non ancora aperta. Per usarla oggi nel browser, esegui codewhale web sul tuo computer."
    ],
    [
      "Desktop",
      "Build di sviluppo",
      "Un’app nativa con lo stesso motore. Rivedi e committa le modifiche, segui il lavoro tra progetti e modifica un messaggio per inviarlo di nuovo come un nuovo ramo. Non è ancora disponibile un download pubblico."
    ]
  ],
  availabilityNote:
    "Il terminale, il browser locale e la GUI non richiedono un account Codewhale. Se usi la tua chiave del provider, è quel provider a fatturarti l’utilizzo.",
  surfacesHeading: "Lavora con file e strumenti",
  surfaces: [
    [
      "File e terminale",
      "Crea file, esegui comandi, analizza dati e testa ciò che realizzi. Tu imposti la cartella di lavoro e i permessi."
    ],
    [
      "Plugin e app connesse",
      "Aggiungi competenze e strumenti con plugin e MCP. Esamina e abilita le connessioni che vuoi far usare all’agente."
    ],
    [
      "Browser e computer · anteprima",
      "Usa gli strumenti browser e il plugin Computer Use per lavorare su app e siti con l’accesso che concedi."
    ],
    [
      "Sessioni da riprendere",
      "Conserva insieme conversazione, risultati degli strumenti e cronologia. Riprendi l’attività nel terminale o nel client browser locale."
    ],
    [
      "Team di agenti",
      "Dividi un lavoro più grande con Fleet tra agenti con ruoli e modelli diversi e seguine i progressi in un solo posto."
    ]
  ],
  runtimeLink: "Esplora strumenti e integrazioni",
  installBandHeading: "Installa su macOS o Linux",
  copy: "Copia",
  copied: "Copiato ✓",
  binaries: "Binari",
  chinaMirrors: "Mirror in Cina",
  installGuideLink: "Leggi la guida d'installazione",
  communityHeading: "Contribuisci a Codewhale",
  communityBody:
    "{count} persone hanno contribuito a creare Codewhale. Segnala un bug, migliora la documentazione o contribuisci con codice su GitHub.",
  communityLinksAria: "Link della community",
  contribute: "Contribuisci su GitHub",
};
