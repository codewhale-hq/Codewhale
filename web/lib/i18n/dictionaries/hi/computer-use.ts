import type { ComputerUseDict } from "../types";

/**
 * Hindi dictionary for `app/[locale]/computer-use/page.tsx` and the
 * Computer Use section of the install page. Product names, the menu items
 * Pause, Stop and Check for updates, and the macOS setting names stay as
 * the app shows them.
 */
export const computerUse: ComputerUseDict = {
  metaTitle: "Computer Use · पूर्वावलोकन · Codewhale",
  metaDescription: "Mac के लिए Codewhale Computer Use डाउनलोड और सेट अप करें। बैकग्राउंड में ऐप नियंत्रण, अनुमतियों का सेटअप और आपके हाथ में Pause व Stop नियंत्रण।",
  title: "Computer Use",
  lead: "आप कंप्यूटर का इस्तेमाल जारी रखें और अपने एजेंट को Mac ऐप्स में काम करने दें।",
  publisher: "Codewhale द्वारा",
  download: "Mac के लिए डाउनलोड करें",
  downloadZip: "ZIP आर्काइव (ऐप के भीतर अपडेटर इसका उपयोग करता है)",
  requirements: "macOS 13.5 या उसके बाद का · Apple silicon और Intel",
  included: "एक ही ऐप डाउनलोड। Node या कंपाइलर अलग से इंस्टॉल करने की ज़रूरत नहीं।",
  pendingTitle: "Mac डाउनलोड तैयार किया जा रहा है",
  pendingBody: "Apple नोटराइज़ेशन और रिलीज़ जाँच पूरी होने के बाद सार्वजनिक इंस्टॉलर यहाँ दिखाई देगा।",
  unavailableTitle: "डाउनलोड की उपलब्धता जाँची नहीं जा सकी",
  unavailableBody: "फिर से कोशिश करने के लिए इस पेज को रीफ़्रेश करें, या नीचे प्रकाशित रिलीज़ देखें।",
  releases: "प्रकाशित रिलीज़",
  receipt: "डाउनलोड सत्यापन विवरण",
  setup: "अपना Mac सेट अप करें",
  steps: [
    { title: "ऐप इंस्टॉल करें", body: "डिस्क इमेज खोलें और Codewhale Computer Use को “ऐप्लिकेशन” में खींचकर छोड़ें। इसे “ऐप्लिकेशन” से खोलें, फिर मेनू बार में व्हेल के आइकन से Computer Use चुनें।" },
    { title: "अनुमतियों की समीक्षा करें", body: "सेटअप बटनों से सिस्टम सेटिंग्ज़ में “ऐक्सेसिबिलिटी” और “स्क्रीन रिकॉर्डिंग” खोलें। कौन-सी अनुमतियाँ देनी हैं, यह आप तय करते हैं।" },
    { title: "अपना एजेंट कनेक्ट करें", body: "अपने CLI संस्करण के लिए नीचे दिया गया Codewhale सेटअप चुनें। अन्य संगत MCP होस्ट के लिए रिपॉज़िटरी की सेटअप मार्गदर्शिका देखें।" },
  ],
  controlsTitle: "नियंत्रण आपके हाथ में",
  controlsBody: "macOS की अनुमतियाँ देने के बाद मानक मोड हर ऐप के लिए अनुमति माँगे बिना काम करता है। आपके प्रतिबंधित ऐप पहुँच से बाहर रहते हैं; Pause कंप्यूटर का इस्तेमाल रोकता है और Stop computer use मौजूदा सेशन समाप्त करता है। सख्त स्वीकृति मोड वैकल्पिक है। काम देखना चाहें, तो लाइव प्रीव्यू माँगें।",
  updateTitle: "अपडेट तब, जब आप चाहें",
  updateBody: "ऐप में Check for updates चुनें। अपडेट इंस्टॉल करने से पहले यह डाउनलोड, Codewhale हस्ताक्षर और Apple नोटराइज़ेशन की जाँच करता है, और रिकवरी के लिए पिछला ऐप सुरक्षित रखता है।",
  help: "सेटअप और समस्या-समाधान",
  notes: "रिलीज़ नोट्स",
  demo: "बैकग्राउंड जाँच देखें",
  source: "GitHub पर देखें",
  platforms: "Mac हेल्पर बीटा में है और मार्केटप्लेस प्लगइन केवल Mac का समर्थन करता है। प्रकाशित संस्करणों में Windows x64 का बिना हस्ताक्षर वाला प्रयोगात्मक प्रीव्यू भी है; Linux के लिए सोर्स या Docker से प्रयोगात्मक सेटअप चाहिए। Windows और Linux पर इनपुट साझा डेस्कटॉप का उपयोग करता है।",
  connectTitle: "Codewhale कनेक्ट करें",
  connectVersion: "Codewhale 0.10.2 में Computer Use का पुराना प्लगइन शामिल है। कैटलॉग की प्रविष्टि इंस्टॉल करने से वह अपडेट नहीं होता। इंस्टॉल किए गए Mac सहायक से सीधे MCP के ज़रिए कनेक्ट करें।",
  connectLimit: "सीधा MCP कनेक्शन टूल जोड़ता है, प्लगइन स्किल नहीं। इसे पुराने अंतर्निर्मित प्लगइन के साथ न चलाएँ। यदि वह सक्षम है, तो पहले /plugin disable computer-use चलाएँ।",
  connectRuntime: "ऐप में Node शामिल है; उसे अलग से इंस्टॉल करने की ज़रूरत नहीं है। यदि ऐप कहीं और इंस्टॉल है, तो APP में ऐप बंडल का वास्तविक पथ दें।",
  connectReload: "चल रहे टर्मिनल सत्र में /mcp reload चलाएँ और यह कहें:",
  connectPrompt: "request_access से Computer Use की अनुमतियाँ जाँचें। कोई इनपुट न भेजें।",
  connectExpected: "जवाब में via: app, सहायक का संस्करण और अनुपलब्ध अनुमतियाँ देखें। यदि सहायक नहीं मिलता, तो रुकें और सेटअप संकेत का पालन करें। इससे केवल कनेक्शन और अनुमतियाँ जाँची जाती हैं।",
  connectGuide: "MCP सेटअप मार्गदर्शिका",
  contributeTitle: "Computer Use को बेहतर बनाने में मदद करें",
  contributeBody: "दस्तावेज़ सुधारें या किसी ऐप की कार्यप्रणाली जाँचें और दोहराए जा सकने वाले परिणाम साझा करें।",
  contributeGuide: "योगदान मार्गदर्शिका",
  contributeIssue: "समस्या रिपोर्ट करें",
};
