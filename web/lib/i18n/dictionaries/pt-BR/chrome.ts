import type { ChromeDict } from "../types";

/**
 * Dicionário pt-BR do chrome — reescrita nativa que espelha o inglês atual.
 * Os rótulos primários da navegação ficam em português e os secundários, em
 * inglês curto (o inverso do dispositivo editorial em Han da edição de
 * referência). O selo 深 do wordmark é uma marca compartilhada.
 */
export const chrome: ChromeDict = {
  navDocs: "Documentação",
  navContribute: "Contribuir",
  navSignIn: "Entrar",
  navDesktop: "Desktop",

  navFeatures: "Capacidades",
  navModels: "Modelos",
  navPlugins: "Plugins",

  skipToContent: "Pular para o conteúdo principal",

  navPrimaryAria: "Navegação principal",
  navHomeAria: "Início do Codewhale",

  installCta: "Instalar →",

  dateLocale: "pt-BR",

  menuOpen: "Abrir menu",
  menuClose: "Fechar menu",
  translationMachineDraft: "Esta tradução é um rascunho automático que ainda não foi revisado por um falante nativo.",

  themeAuto: "auto",
  themeLight: "claro",
  themeDark: "escuro",
  themeAria: "Tema: {mode} (clique para alternar)",
  themeTitle: "Tema · auto / claro / escuro",

  footerProduct: "Produto",
  footerProject: "Projeto",
  footerDocs: "Documentação",
  footerGuide: "Primeiros passos",
  footerInstall: "Instalação",
  footerModels: "Modelos",
  footerFaq: "Perguntas frequentes",
  footerContribute: "Contribuir",
  footerRoadmap: "Roadmap",
  footerLicense: "Licença MIT",
  footerMerch: "Produtos",
  footerTerms: "Termos de serviço",
  footerPrivacy: "Privacidade",
  footerChangelog: "Registro de alterações",
  footerCanonicalSource: "Fonte canônica: ",
  footerReleases: " · Lançamentos: ",
  footerReleasesLink: "Lançamentos no GitHub",
  footerSecurity: "Segurança",
  footerLegal: "Legal",

  switcherLabel: "Idioma",
  switcherSwitchTo: "Mudar para {label}",
  partialBadge: "(parcial)",

  stateReleased: "Lançado",
  stateAvailable: "Disponível",
  stateBeta: "Beta",
  stateDev: "Versão de desenvolvimento",
  stateOpen: "Aberto",
  stateInviteOnly: "Somente por convite",
  stateNotOpen: "Ainda não aberto",
  stateNotPublic: "Não público",
  stateInSource: "No código-fonte, ainda não lançado",
  numbersAsOf: "Em {date}",
  downloadMacos: "Baixar para macOS",
  downloadVersion: "Versão {version}",
  downloadSize: "Tamanho {size}",
  downloadChecksum: "Soma de verificação SHA-256",
  downloadSigned: "Assinado e notarizado",
};
