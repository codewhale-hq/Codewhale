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
  lead: "Lascia che Codewhale lavori nelle tue app mentre continui a lavorare. Autorizzalo una volta su questo Mac e interrompi l’uso del computer quando vuoi.",
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
    { title: "Attiva Computer Use", body: "Nelle impostazioni Connectors di Codewhale, installa o aggiorna Computer Use, poi esaminalo e attivalo. Chiedigli di lavorare in un’app. Al primo utilizzo, scegli Trust until revoked nell’helper nativo; la fiducia resta valida passando tra app, modelli e attività." },
  ],
  controlsTitle: "Mantieni il controllo",
  controlsBody: "Il controllo compatto di Codewhale appare accanto all’app in uso. Pause sospende l’uso del computer; Stop computer use termina tutte le sessioni attuali del computer. Scegli Turn off nell’helper per rimuovere l’accesso di Codewhale. Chiedi un’anteprima dal vivo quando vuoi seguire le operazioni.",
  updateTitle: "Aggiornamenti quando decidi tu",
  updateBody: "Scegli Check for updates (Controlla aggiornamenti) nell’app. Prima di installare un aggiornamento, l’helper verifica il file scaricato, la firma Codewhale e la notarizzazione Apple, e conserva la versione precedente per il ripristino.",
  help: "Configurazione e risoluzione dei problemi",
  notes: "Note di rilascio",
  demo: "Guarda il test in background",
  source: "Codice sorgente e altre piattaforme",
  platforms: "Questo download è per Mac. Windows e Linux usano al momento il plugin da sorgente e la configurazione lato host.",
};
