import type { ComputerUseDict } from "../types";

/**
 * Italian dictionary for `app/[locale]/computer-use/page.tsx` and the
 * Computer Use section of the install page. Product names, the menu items
 * Pause, Stop and Check for updates, and the macOS setting names stay as
 * the app shows them.
 */
export const computerUse: ComputerUseDict = {
  metaTitle: "Computer Use · Anteprima · Codewhale",
  metaDescription: "Scarica e configura Codewhale Computer Use per Mac. Controllo delle app in background, configurazione dei permessi e comandi Pause e Stop sotto il tuo controllo.",
  title: "Computer Use",
  lead: "Lascia che il tuo agente lavori nelle app del Mac mentre continui a usare il computer.",
  publisher: "Di Codewhale",
  download: "Scarica per Mac",
  downloadZip: "Archivio ZIP (usato dall’aggiornamento in-app)",
  requirements: "macOS 13.5 o successivo · Apple silicon e Intel",
  included: "Un solo download. Non servono né un’installazione separata di Node né un compilatore.",
  pendingTitle: "Download per Mac in preparazione",
  pendingBody: "L’installer pubblico comparirà qui al termine della notarizzazione Apple e dei controlli di rilascio.",
  unavailableTitle: "Impossibile verificare la disponibilità del download",
  unavailableBody: "Ricarica la pagina per riprovare, oppure consulta le release pubblicate qui sotto.",
  releases: "Release pubblicate",
  receipt: "Dettagli di verifica del download",
  setup: "Configura il tuo Mac",
  steps: [
    { title: "Installa l’app", body: "Apri l’immagine disco e trascina Codewhale Computer Use in “Applicazioni”. Aprila da “Applicazioni”, poi scegli Computer Use dall’icona della balena nella barra dei menu." },
    { title: "Controlla i permessi", body: "Usa i pulsanti di configurazione per aprire “Accessibilità” e “Registrazione schermo” nelle Impostazioni di Sistema. Decidi tu quali permessi concedere." },
    { title: "Collega il tuo agente", body: "Scegli qui sotto la configurazione di Codewhale per la tua versione della CLI. Gli altri client MCP compatibili possono usare la guida di configurazione del repository." },
  ],
  controlsTitle: "Mantieni il controllo",
  controlsBody: "Dopo aver concesso i permessi macOS, la modalità standard funziona senza chiedere l’autorizzazione per ogni app. Le app che blocchi restano inaccessibili; Pause sospende l’uso del computer e Stop computer use termina le sessioni attuali. L’approvazione rigorosa è facoltativa. Chiedi un’anteprima dal vivo per seguire le operazioni.",
  updateTitle: "Aggiornamenti quando decidi tu",
  updateBody: "Scegli Check for updates (Controlla aggiornamenti) nell’app. Prima di installare un aggiornamento, l’helper verifica il file scaricato, la firma Codewhale e la notarizzazione Apple, e conserva la versione precedente per il ripristino.",
  help: "Configurazione e risoluzione dei problemi",
  notes: "Note di rilascio",
  demo: "Guarda il test in background",
  source: "Visualizza su GitHub",
  platforms: "L’helper per Mac è in beta e il plugin del catalogo supporta solo Mac. Le versioni pubblicate includono anche un’anteprima sperimentale non firmata per Windows x64; Linux richiede una configurazione sperimentale dai sorgenti o con Docker. L’input su Windows e Linux usa il desktop condiviso.",
  connectTitle: "Collega Codewhale",
  connectVersion: "Codewhale 0.10.2 include una vecchia versione del plugin Computer Use. Installare la voce del catalogo non lo aggiorna. Collega direttamente l’app di supporto Mac installata tramite MCP.",
  connectLimit: "La connessione MCP diretta aggiunge strumenti senza le skill del plugin. Non usarla insieme al vecchio plugin integrato. Se è attivo, esegui prima /plugin disable computer-use.",
  connectRuntime: "L’app include Node; non serve installarlo separatamente. Se è installata altrove, imposta APP sul percorso effettivo del bundle dell’app.",
  connectReload: "In una sessione del terminale in esecuzione, esegui /mcp reload e chiedi:",
  connectPrompt: "Controlla le autorizzazioni di Computer Use con request_access. Non inviare alcun input.",
  connectExpected: "Controlla via: app, la versione dell’app di supporto e le autorizzazioni mancanti nella risposta. Se l’app di supporto non è presente, fermati e segui le indicazioni di configurazione. Questo verifica solo la connessione e le autorizzazioni.",
  connectGuide: "Guida alla configurazione MCP",
  contributeTitle: "Aiuta a migliorare Computer Use",
  contributeBody: "Migliora la documentazione o prova un flusso di lavoro in un’app e condividi risultati riproducibili.",
  contributeGuide: "Guida ai contributi",
  contributeIssue: "Segnala un problema",
};
