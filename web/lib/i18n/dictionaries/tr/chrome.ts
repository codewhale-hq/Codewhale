import type { ChromeDict } from "../types";

/**
 * Turkish chrome dictionary.
 *
 * Güncel İngilizce yönü yansıtan özgün yeniden yazım — «istediğin model,
 * senin makinen»; emekli «local-first» konumlandırması yok. Türkçe
 * geliştirici topluluğunun alışıldığı samimi «sen» dili.
 *
 * Modlar ve izin duruşları (posture) literal kalır (Plan / Work / Operate,
 * Ask / Auto-Review / Full Access); `Runtime`, `fleet` ve `TUI` ürün adı
 * olarak kalır, «makbuz» receipt karşılığıdır.
 *
 * İkincil gezinme etiketleri Türkçe birinciyi kısa bir İngilizce eşle
 * eşler — Han çifti İngilizce baskının kendi editoryal aracıdır.
 */
export const chrome: ChromeDict = {
  navDocs: "Belgeler",
  navContribute: "Katkı",
  navSignIn: "Giriş yap",
  navDesktop: "Masaüstü",

  navFeatures: "Özellikler",
  navModels: "Modeller",
  navPlugins: "Eklentiler",

  skipToContent: "Ana içeriğe geç",

  navPrimaryAria: "Ana gezinme",
  navHomeAria: "Codewhale ana sayfası",

  installCta: "Kur →",

  dateLocale: "tr-TR",

  menuOpen: "Menüyü aç",
  menuClose: "Menüyü kapat",
  translationMachineDraft: "Bu çeviri makine taslağıdır ve henüz anadili konuşan biri tarafından gözden geçirilmedi.",

  themeAuto: "otomatik",
  themeLight: "açık",
  themeDark: "koyu",
  themeAria: "Tema: {mode} (geçiş için tıkla)",
  themeTitle: "Tema · otomatik / açık / koyu",

  footerProduct: "Ürün",
  footerProject: "Proje",
  footerDocs: "Belgeler",
  footerGuide: "Başlangıç",
  footerInstall: "Kurulum",
  footerModels: "Modeller",
  footerFaq: "SSS",
  footerContribute: "Katkı",
  footerRoadmap: "Yol haritası",
  footerLicense: "MIT lisansı",
  footerMerch: "Ürünler",
  footerTerms: "Hizmet şartları",
  footerPrivacy: "Gizlilik",
  footerChangelog: "Değişiklik günlüğü",
  footerCanonicalSource: "Yetkili kaynak: ",
  footerReleases: " · Sürümler: ",
  footerReleasesLink: "GitHub sürümleri",
  footerSecurity: "Güvenlik",
  footerLegal: "Yasal",

  switcherLabel: "Dil",
  switcherSwitchTo: "{label} diline geç",
  partialBadge: "(kısmi)",

  stateReleased: "Yayımlandı",
  stateAvailable: "Kullanılabilir",
  stateBeta: "Beta",
  stateDev: "Geliştirme sürümü",
  stateOpen: "Açık",
  stateInviteOnly: "Yalnızca davetle",
  stateNotOpen: "Henüz açık değil",
  stateNotPublic: "Herkese açık değil",
  stateInSource: "Kaynakta var, henüz yayımlanmadı",
  numbersAsOf: "{date} itibarıyla",
  downloadMacos: "macOS için indir",
  downloadVersion: "Sürüm {version}",
  downloadSize: "Boyut {size}",
  downloadChecksum: "SHA-256 sağlama toplamı",
  downloadSigned: "İmzalı ve noter onaylı",
};
