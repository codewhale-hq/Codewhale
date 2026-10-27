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
  lead: "Siz çalışmaya devam ederken Codewhale uygulamalarınızda çalışsın. Bu Mac’te bir kez izin verin ve bilgisayar kullanımını istediğiniz zaman durdurun.",
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
    { title: "Computer Use’u etkinleştir", body: "Codewhale’in Connectors ayarlarında Computer Use’u yükle veya güncelle, ardından incele ve etkinleştir. Bir uygulamada çalışmasını iste. İlk kullanımda yerel yardımcıda Trust until revoked seçeneğini seç; bu güven uygulama, model ve görev değiştiğinde de geçerli kalır." },
  ],
  controlsTitle: "Kontrol sende kalsın",
  controlsBody: "Kullanılan uygulamanın yanında küçük bir Codewhale kontrolü görünür. Pause bilgisayar kullanımını duraklatır; Stop computer use mevcut tüm bilgisayar oturumlarını sonlandırır. Codewhale’in erişimini kaldırmak için yardımcıda Turn off’u seç. Çalışmasını izlemek istediğinde canlı önizleme iste.",
  updateTitle: "Güncellemeler sen istediğinde",
  updateBody: "Uygulamadan Check for updates'i (Güncellemeleri denetle) seç. Bir güncellemeyi kurmadan önce indirmeyi, Codewhale imzasını ve Apple noter onayını doğrular; kurtarma için önceki uygulamayı saklar.",
  help: "Kurulum ve sorun giderme",
  notes: "Sürüm notları",
  demo: "Arka plan kontrolünü izle",
  source: "Kaynak kodu ve diğer platformlar",
  platforms: "Bu indirme Mac içindir. Windows ve Linux şu anda kaynak eklentisini ve ana makine tarafındaki kurulumu kullanır.",
};
