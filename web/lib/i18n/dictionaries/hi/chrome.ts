import type { ChromeDict } from "../types";

/**
 * Hindi chrome dictionary.
 *
 * मानक हिन्दी, तुरंत-बरती तकनीकी शब्दावली के साथ — TUI पैक
 * (crates/tui/locales/hi.json) के रजिस्टर से मेल खाता हुआ: «समीक्षा करें»,
 * «चलाएँ»। अंग्रेज़ी की वर्तमान दिशा दर्शाती मूल पुनर्लेखन — कोई भी मॉडल,
 * आपकी मशीन पर।
 *
 * मोड और अनुमति-मुद्रा (posture) literal रहते हैं (Plan / Work / Operate,
 * Ask / Auto-Review / Full Access); `Runtime`, `fleet`, `TUI` उत्पाद-नाम
 * हैं और ऐसे ही रहते हैं।
 *
 * सेकेंडरी नेविगेशन लेबल हिन्दी मुख्य लेबल के साथ छोटा अंग्रेज़ी साथी
 * रखते हैं — हन जोड़ी अंग्रेज़ी संस्करण का अपना संपादकीय उपकरण है।
 */
export const chrome: ChromeDict = {
  navDocs: "दस्तावेज़ीकरण",
  navContribute: "योगदान",
  navSignIn: "साइन इन",
  navDesktop: "डेस्कटॉप",

  navFeatures: "क्षमताएँ",
  navModels: "मॉडल",
  navPlugins: "प्लगइन",

  skipToContent: "मुख्य सामग्री पर जाएँ",

  navPrimaryAria: "मुख्य नेविगेशन",
  navHomeAria: "Codewhale होम",

  installCta: "इंस्टॉल करें →",

  dateLocale: "hi-IN",

  menuOpen: "मेनू खोलें",
  menuClose: "मेनू बंद करें",
  translationMachineDraft: "यह अनुवाद मशीन द्वारा बनाया गया मसौदा है और इसकी अभी किसी मूल भाषी द्वारा समीक्षा नहीं हुई है।",

  themeAuto: "ऑटो",
  themeLight: "लाइट",
  themeDark: "डार्क",
  themeAria: "थीम: {mode} (बदलने के लिए क्लिक करें)",
  themeTitle: "थीम · ऑटो / लाइट / डार्क",

  footerProduct: "उत्पाद",
  footerProject: "प्रोजेक्ट",
  footerDocs: "दस्तावेज़ीकरण",
  footerGuide: "शुरुआत कैसे करें",
  footerInstall: "इंस्टॉल",
  footerModels: "मॉडल",
  footerFaq: "सामान्य प्रश्न",
  footerContribute: "योगदान दें",
  footerRoadmap: "रोडमैप",
  footerLicense: "MIT लाइसेंस",
  footerMerch: "मर्चेंडाइज़",
  footerTerms: "सेवा की शर्तें",
  footerPrivacy: "गोपनीयता",
  footerChangelog: "परिवर्तन लॉग",
  footerCanonicalSource: "कैननिकल सोर्स: ",
  footerReleases: " · रिलीज़: ",
  footerReleasesLink: "GitHub रिलीज़",
  footerSecurity: "सुरक्षा",
  footerLegal: "कानूनी",

  switcherLabel: "भाषा",
  switcherSwitchTo: "{label} पर स्विच करें",
  partialBadge: "(आंशिक)",

  stateReleased: "जारी किया गया",
  stateAvailable: "उपलब्ध",
  stateBeta: "बीटा",
  stateDev: "डेवलपमेंट बिल्ड",
  stateOpen: "खुला",
  stateInviteOnly: "केवल आमंत्रण से",
  stateNotOpen: "अभी खुला नहीं है",
  stateNotPublic: "सार्वजनिक नहीं",
  stateInSource: "स्रोत में है, अभी जारी नहीं हुआ",
  numbersAsOf: "{date} तक",
  downloadMacos: "macOS के लिए डाउनलोड करें",
  downloadVersion: "संस्करण {version}",
  downloadSize: "आकार {size}",
  downloadChecksum: "SHA-256 चेकसम",
  downloadSigned: "हस्ताक्षरित और नोटराइज़्ड",
  downloadLinux: "Linux",
  downloadAptTitle: "apt से इंस्टॉल करें",
  downloadAptTrust: "apt रिपॉज़िटरी पर Codewhale की कुंजी से हस्ताक्षर हैं, और अपडेट apt से इंस्टॉल होते हैं।",
  downloadAppImage: "AppImage डाउनलोड करें",
  downloadAppImageTrust: "दिखाए गए SHA-256 चेकसम से AppImage की जाँच करें। यह अभी अपने आप अपडेट नहीं होता।",
  downloadAptFingerprint: "हस्ताक्षर कुंजी का फ़िंगरप्रिंट {fingerprint} होना चाहिए। इसे डाउनलोड की गई कुंजी से मिलाएँ।",
};
