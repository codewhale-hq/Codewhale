import type { ComputerUseDict } from "../types";

/**
 * Turkish dictionary for `app/[locale]/computer-use/page.tsx` and the
 * Computer Use section of the install page. Product names, the menu items
 * Pause, Stop and Check for updates, and the macOS setting names stay as
 * the app shows them.
 */
export const computerUse: ComputerUseDict = {
  metaTitle: "Computer Use · Önizleme · Codewhale",
  metaDescription: "Mac için Codewhale Computer Use'u indir ve kur. Arka planda uygulama denetimi, izin kurulumu ve senin elindeki Pause ve Stop denetimleri.",
  title: "Computer Use",
  lead: "Siz bilgisayarınızı kullanmaya devam ederken ajanınız Mac uygulamalarında çalışsın.",
  publisher: "Codewhale tarafından",
  download: "Mac için indir",
  downloadZip: "ZIP arşivi (uygulama içi güncelleyici kullanır)",
  requirements: "macOS 13.5 veya üzeri · Apple silicon ve Intel",
  included: "Tek bir uygulama indirmesi. Ayrıca Node kurulumu veya derleyici gerekmez.",
  pendingTitle: "Mac indirmesi hazırlanıyor",
  pendingBody: "Apple noter onayı ve sürüm kontrolleri tamamlandığında herkese açık yükleyici burada görünecek.",
  unavailableTitle: "İndirme durumu kontrol edilemedi",
  unavailableBody: "Yeniden denemek için sayfayı yenile ya da aşağıdaki yayımlanmış sürümlere bak.",
  releases: "Yayımlanmış sürümler",
  receipt: "İndirme doğrulama bilgileri",
  setup: "Mac'ini kur",
  steps: [
    { title: "Uygulamayı kur", body: "Disk görüntüsünü aç ve Codewhale Computer Use'u Uygulamalar klasörüne sürükle. Uygulamalar'dan aç, ardından menü çubuğundaki balina simgesinden Computer Use'u seç." },
    { title: "İzinleri gözden geçir", body: "Kurulum düğmeleriyle Sistem Ayarları'nda Erişilebilirlik ve Ekran Kaydı bölümlerini aç. Hangi izinleri vereceğine sen karar verirsin." },
    { title: "Ajanınızı bağlayın", body: "Codewhale’de /plugin marketplace install codewhale computer-use ile Computer Use’u yükleyin, ardından eklentiyi inceleyip etkinleştirin. Yardımcı ayrı olarak yüklenir. Diğer uyumlu MCP uygulamaları, kurulum kılavuzunu izleyerek yardımcıya bağlanır." },
  ],
  controlsTitle: "Kontrol sende kalsın",
  controlsBody: "macOS izinleri verildikten sonra standart mod, her uygulama için izin istemeden çalışır. Engellediğiniz uygulamalar erişime kapalı kalır; Pause bilgisayar kullanımını duraklatır, Stop computer use mevcut oturumları sonlandırır. Sıkı onay modu isteğe bağlıdır. Çalışmayı izlemek istediğinizde canlı önizleme isteyin.",
  updateTitle: "Güncellemeler sen istediğinde",
  updateBody: "Uygulamadan Check for updates'i (Güncellemeleri denetle) seç. Bir güncellemeyi kurmadan önce indirmeyi, Codewhale imzasını ve Apple noter onayını doğrular; kurtarma için önceki uygulamayı saklar.",
  help: "Kurulum ve sorun giderme",
  notes: "Sürüm notları",
  demo: "Arka plan kontrolünü izle",
  source: "Kaynak kodu ve diğer platformlar",
  platforms: "Mac yardımcısı beta sürümündedir ve pazar yeri eklentisi yalnızca Mac’i destekler. Yayımlanan sürümlerde imzasız, deneysel bir Windows x64 önizlemesi de bulunur; Linux için kaynak koddan veya Docker ile deneysel kurulum gerekir. Windows ve Linux girdileri ortak masaüstünü kullanır.",
};
