import { project as en } from "./dictionaries/en/project";
import { project as zh } from "./dictionaries/zh/project";
import { project as ja } from "./dictionaries/ja/project";
import { project as vi } from "./dictionaries/vi/project";
import { project as ko } from "./dictionaries/ko/project";
import { project as ru } from "./dictionaries/ru/project";
import { project as uk } from "./dictionaries/uk/project";
import { project as es } from "./dictionaries/es/project";
import { project as ptBR } from "./dictionaries/pt-BR/project";
import { project as fr } from "./dictionaries/fr/project";
import { project as de } from "./dictionaries/de/project";
import { project as ca } from "./dictionaries/ca/project";
import { project as id } from "./dictionaries/id/project";
import { project as hi } from "./dictionaries/hi/project";
import { project as tr } from "./dictionaries/tr/project";
import { project as it } from "./dictionaries/it/project";
import { project as pl } from "./dictionaries/pl/project";
import { project as ar } from "./dictionaries/ar/project";
import type { LocalizedText } from "../content/vocabulary";

type ProjectKey = keyof typeof en;
const dictionaries: Record<string, Record<ProjectKey, string>> = {
  "en": en,
  "zh": zh,
  "ja": ja,
  "vi": vi,
  "ko": ko,
  "ru": ru,
  "uk": uk,
  "es": es,
  "pt-BR": ptBR,
  "fr": fr,
  "de": de,
  "ca": ca,
  "id": id,
  "hi": hi,
  "tr": tr,
  "it": it,
  "pl": pl,
  "ar": ar,
};

export function projectText(key: ProjectKey): LocalizedText {
  return {
    ...Object.fromEntries(Object.entries(dictionaries).map(([locale, dictionary]) => [locale, dictionary[key]])),
    en: en[key],
    zh: zh[key],
  };
}
