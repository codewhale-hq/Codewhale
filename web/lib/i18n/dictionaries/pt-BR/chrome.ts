import type { ChromeDict } from "../types";

/**
 * Dicionário pt-BR do chrome — reescrita nativa que espelha o inglês atual.
 * Os rótulos primários da navegação ficam em português e os secundários, em
 * inglês curto (o inverso do dispositivo editorial em Han da edição de
 * referência). O selo 深 do wordmark é uma marca compartilhada.
 */
export const chrome: ChromeDict = {
  navDocs: "Documentação",
  navStart: "Começar",
  navInstall: "Instalar",
  navFaq: "Dúvidas",
  navCommunity: "Comunidade",
  navContribute: "Contribuir",

  navFeatures: "Capacidades",
  navProduct: "Produto",
  navModels: "Modelos",
  navPlugins: "Plugins",

  skipToContent: "Pular para o conteúdo principal",

  navPrimaryAria: "Navegação principal",
  navHomeAria: "Início do Codewhale",

  installCta: "Instalar →",

  dateLocale: "pt-BR",

  menuOpen: "Abrir menu",
  menuClose: "Fechar menu",

  themeAuto: "auto",
  themeLight: "claro",
  themeDark: "escuro",
  themeAria: "Tema: {mode} (clique para alternar)",
  themeTitle: "Tema · auto / claro / escuro",

  footerTagline:
    "O agente de programação de código aberto para seu terminal e os modelos que você escolher.",
  footerProduct: "Produto",
  footerProject: "Projeto",
  footerDocs: "Documentação",
  footerGuide: "Primeiros passos",
  footerInstall: "Instalação",
  footerModels: "Modelos",
  footerRuntime: "Runtime",
  footerFaq: "Perguntas frequentes",
  footerIssues: "Issues",
  footerContribute: "Contribuir",
  footerRoadmap: "Roadmap",
  footerLicense: "Licença MIT",
  footerTerms: "Termos de serviço",
  footerPrivacy: "Privacidade",
  footerChangelog: "Registro de alterações",
  footerCanonicalSource: "Fonte canônica: ",
  footerReleases: " · Lançamentos: ",
  footerReleasesLink: "Lançamentos no GitHub",
  footerSecurity: "Segurança",

  switcherLabel: "Idioma",
  switcherSwitchTo: "Mudar para {label}",
  partialBadge: "(parcial)",
};
