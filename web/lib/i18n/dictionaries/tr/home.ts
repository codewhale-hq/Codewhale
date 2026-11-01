import type { HomeDict } from "../types";

/** Turkish home copy: useful work with chosen models and connected tools. */

export const home: HomeDict = {
  metaTitle: "Codewhale: açık kaynak kodlama ajanı",
  metaDescription: "Codewhale, terminaliniz için açık kaynak bir kodlama ajanıdır. Kendi barındırılan veya yerel modellerinizle projeleri okuyun, düzenleyin ve test edin.",
  heroTitle: "Her model için açık kaynak kodlama ajanı",
  heroIntro: "{brand} terminalinden projeni okur, dosyaları düzenler ve testlerini çalıştırır. Barındırılan veya yerel bir model bağla ve hangi işlemlerin onayını gerektirdiğini seç.",
  getCodewhale: "Codewhale'i kur",
  heroInstallAria: "Kurulum komutu",
  exploreProduct: "Codewhale’i keşfet",
  shotPreview: "Terminal önizlemesi",
  screenshotAlt:
    "Codewhale v{version}: sohbeti, mesaj alanını ve oturum kontrollerini gösteren terminal kaydı.",
  latestRelease: "En yeni sürüm {tag}",
  releaseUnavailable: "Sürüm durumu kullanılamıyor",
  proofContributors: "{count} katkıda bulunan",
  currentSource: "Kaynak",
  sourceCandidate: "Yayımlanmadı",
  publishedRelease: "yayımlandı",
  gainHeading:
    "Terminalinden geliştir ve otomatikleştir",
  gainLede:
    "Ne geliştirmek veya otomatikleştirmek istediğini anlat. Codewhale, erişimini senin belirlediğin sınırlar içinde dosyaları düzenleyebilir, komut çalıştırabilir ve sonucu kontrol edebilir.",
  gain: [
    [
      "Uygulamalar ve araçlar geliştir",
      "Bir uygulama geliştir, bir özellik ekle veya bir betik yaz. Codewhale proje dosyaları üzerinde çalışabilir, kodu çalıştırabilir ve geliştirdiği şeyi test edebilir."
    ],
    [
      "Tekrarlanan işleri otomatikleştir",
      "İş akışlarını terminalinden, betiklerden veya CI üzerinden çalıştır. Daha büyük görevlerde işin bölümlerini farklı modeller kullanan bir Fleet ajan ekibine devret."
    ],
    [
      "Kullandığın araçları bağla",
      "Eklentiler ve MCP sunucuları aracılığıyla araçlar ekle veya kendi betiklerinden API’leri kullan. Her hizmet kendi kurulumunu ve kimlik doğrulamasını gerektirir."
    ]
  ],
  chapterModels: "Senin modellerin",
  modelsHeading: "Seçtiğin modelleri kullan",
  modelsBody:
    "Sağlayıcı hesaplarını, OpenAI uyumlu bir uç noktayı veya yerel ve kendi barındırdığın modelleri bağla. Oturum ve Fleet içindeki her ajan için bir model seç.",
  modelsFacts: [
    [
      "API hesapların",
      "DeepSeek, OpenAI, Anthropic veya Google gibi sağlayıcıları kendi anahtarlarınla bağla. Varsayılan model deepseek-flash artık DeepSeek-V4.1-Flash kullanıyor."
    ],
    [
      "Ağ geçidin",
      "OpenAI uyumlu bir uç nokta kullan ve sunduğu modelleri seç."
    ],
    [
      "Kendi çıkarımın",
      "Ollama, vLLM veya SGLang ile yerel ya da kendi barındırdığın modelleri çalıştır."
    ],
    [
      "ChatGPT planın",
      "API anahtarı yerine ChatGPT planı kullanmak için codewhale auth chatgpt ile oturum aç. Deneysel."
    ]
  ],
  modelsLink: "Modellere ve sağlayıcılara göz at",
  startHeading: "Başlarken",
  startLede:
    "Codewhale’i yükle, bir model bağla ve bir proje klasörü aç. İhtiyaç duydukça eklentiler ve daha fazla ajan ekleyebilirsin.",
  startGuideLink: "Başlangıç kılavuzunu takip et",
  startVocabularyLink: "Ürün sözlüğünü gör",
  chapterAvailability: "Nerede çalışır",
  availabilityHeading: "Şimdi kullanılabilenler ve geliştirilmekte olanlar",
  availabilityLede:
    "Terminal ve yerel tarayıcı istemcisi hazır. Yerel masaüstü uygulaması ve yeniden kurulan barındırılmış web uygulaması geliştiriliyor.",
  availability: [
    [
      "Terminal ve yerel tarayıcı",
      "Yayınlandı",
      "Linux, macOS veya Windows üzerine kur, ardından codewhale komutunu ya da yerel tarayıcı istemcisi için codewhale web komutunu çalıştır. npm ve Cargo da çalışır; Termux üzerinde Android önizleme aşamasında."
    ],
    [
      "CodeWhale GUI (VS Code)",
      "Kullanılabilir",
      "Topluluk tarafından sürdürülen ayrı bir proje: aynı Codewhale Runtime üzerinde VS Code kenar çubuğunda sohbet, konular ve dosya değişiklikleri. VS Code Marketplace'ten kur.",
      "https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode"
    ],
    [
      "Web uygulaması",
      "Geliştirme aşamasında",
      "Masaüstü uygulamasına uyacak şekilde yeniden yapılıyor ve henüz açılmadı. Bugün tarayıcıda kullanmak için kendi bilgisayarında codewhale web komutunu çalıştır."
    ],
    [
      "Masaüstü",
      "Geliştirme sürümü",
      "Aynı motoru kullanan yerel bir uygulama. Değişiklikleri inceleyip commit et, projeler arasındaki işleri takip et ve bir mesajı düzenleyerek yeni bir dal olarak yeniden gönder. Henüz herkese açık indirme yok."
    ]
  ],
  availabilityNote:
    "Terminal, yerel tarayıcı ve GUI için Codewhale hesabı gerekmez. Kendi sağlayıcı anahtarını kullanırsan, bu kullanımı sağlayıcın faturalandırır.",
  surfacesHeading: "Dosyalar ve araçlarla çalış",
  surfaces: [
    [
      "Dosyalar ve terminal",
      "Dosya oluştur, komut çalıştır, verileri incele ve geliştirdiklerini test et. Çalışma klasörünü ve izinleri sen belirlersin."
    ],
    [
      "Eklentiler ve bağlı uygulamalar",
      "Eklentiler ve MCP ile beceriler ve araçlar ekle. Ajanın kullanmasını istediğin bağlantıları incele ve etkinleştir."
    ],
    [
      "Tarayıcı ve bilgisayar · önizleme",
      "Verdiğin erişim kapsamında uygulama ve sitelerde tarayıcı araçları ve Computer Use eklentisiyle çalış."
    ],
    [
      "Devam edebileceğin oturumlar",
      "Sohbeti, araç sonuçlarını ve çalışma geçmişini bir arada tut. Görevi terminalde veya yerel tarayıcı istemcisinde sürdür."
    ],
    [
      "Ajan ekipleri",
      "Büyük bir işi Fleet ile farklı rol ve modellerdeki ajanlara böl, ilerlemelerini tek yerde izle."
    ]
  ],
  runtimeLink: "Araçları ve entegrasyonları keşfet",
  installBandHeading: "macOS veya Linux üzerine kur",
  copy: "Kopyala",
  copied: "Kopyalandı ✓",
  binaries: "İkililer",
  chinaMirrors: "Çin yansıları",
  installGuideLink: "Kurulum kılavuzunu oku",
  communityHeading: "Codewhale’e katkıda bulun",
  communityBody:
    "Codewhale'in geliştirilmesine {count} kişi katkıda bulundu. GitHub’da bir hata bildir, belgeleri iyileştir veya kod katkısı yap.",
  communityLinksAria: "Topluluk bağlantıları",
  contribute: "GitHub’da katkıda bulun",
};
