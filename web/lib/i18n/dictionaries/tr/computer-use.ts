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
    { title: "Ajanınızı bağlayın", body: "CLI sürümünüz için aşağıdaki Codewhale kurulumunu seçin. Diğer uyumlu MCP istemcileri için depodaki kurulum rehberini kullanın." },
  ],
  controlsTitle: "Kontrol sende kalsın",
  controlsBody: "macOS izinleri verildikten sonra standart mod, her uygulama için izin istemeden çalışır. Engellediğiniz uygulamalar erişime kapalı kalır; Pause bilgisayar kullanımını duraklatır, Stop computer use mevcut oturumları sonlandırır. Sıkı onay modu isteğe bağlıdır. Çalışmayı izlemek istediğinizde canlı önizleme isteyin.",
  updateTitle: "Güncellemeler sen istediğinde",
  updateBody: "Uygulamadan Check for updates'i (Güncellemeleri denetle) seç. Bir güncellemeyi kurmadan önce indirmeyi, Codewhale imzasını ve Apple noter onayını doğrular; kurtarma için önceki uygulamayı saklar.",
  help: "Kurulum ve sorun giderme",
  notes: "Sürüm notları",
  demo: "Arka plan kontrolünü izle",
  source: "GitHub’da görüntüle",
  platforms: "Mac yardımcısı beta sürümündedir ve pazar yeri eklentisi yalnızca Mac’i destekler. Yayımlanan sürümlerde imzasız, deneysel bir Windows x64 önizlemesi de bulunur; Linux için kaynak koddan veya Docker ile deneysel kurulum gerekir. Windows ve Linux girdileri ortak masaüstünü kullanır.",
  connectTitle: "Codewhale’i bağlayın",
  connectVersion: "Codewhale 0.10.2 eski bir Computer Use eklentisi içerir. Katalog girdisini yüklemek bunu güncellemez. Yüklü Mac yardımcısına MCP üzerinden doğrudan bağlanın.",
  connectLimit: "Doğrudan MCP bağlantısı, eklenti becerileri olmadan araçlar ekler. Eski yerleşik eklentiyle birlikte kullanmayın. Bu eklenti etkinse önce /plugin disable computer-use çalıştırın.",
  connectRuntime: "Uygulama Node içerir; ayrıca yüklemeniz gerekmez. Başka bir konuma yüklendiyse APP değerini gerçek uygulama paketi yoluna ayarlayın.",
  connectReload: "Açık bir terminal oturumunda /mcp reload çalıştırın ve şunu isteyin:",
  connectPrompt: "Computer Use izinlerini request_access ile kontrol et. Hiçbir giriş gönderme.",
  connectExpected: "Yanıtta via: app, yardımcı sürümü ve eksik izinleri kontrol edin. Yardımcı bulunamazsa durun ve kurulum ipucunu izleyin. Bu yalnızca bağlantıyı ve izinleri kontrol eder.",
  connectGuide: "MCP kurulum rehberi",
  contributeTitle: "Computer Use’u geliştirmeye katılın",
  contributeBody: "Belgeleri iyileştirin veya bir uygulama iş akışını test edip tekrarlanabilir sonuçları paylaşın.",
  contributeGuide: "Katkı rehberi",
  contributeIssue: "Sorun bildirin",
};
