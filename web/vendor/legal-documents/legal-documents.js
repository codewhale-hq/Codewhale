/**
 * Publication metadata shared by every Codewhale legal page. This record does
 * not approve legal text or publish a draft; approval and publication require
 * a recorded founder decision before its status can change.
 *
 * Recorded founder decision (chat, 2026-10-04): the paid terms and the privacy
 * revision drafted with them are approved and in effect from October 4, 2026.
 * They move together: the terms name paid checkout, and so must the policy.
 */
export const LEGAL_DOCUMENTS = Object.freeze({
  terms: Object.freeze({ version: "2026-10-04", status: "effective", effectiveAt: "2026-10-04" }),
  privacy: Object.freeze({ version: "2026-10-04", status: "effective", effectiveAt: "2026-10-04" }),
  membership: Object.freeze({ version: "2026-11-18", status: "draft", effectiveAt: null }),
});

/** Render the pinned document's status; a revision date never makes a draft effective. */
export function formatLegalDocumentStatus(documentId, dateLocale = "en-US") {
  if (!Object.hasOwn(LEGAL_DOCUMENTS, documentId)) throw new TypeError("Unknown legal document");
  const document = LEGAL_DOCUMENTS[documentId];
  const date = new Intl.DateTimeFormat(dateLocale, { dateStyle: "long", timeZone: "UTC" })
    .format(new Date(`${document.version}T00:00:00Z`));
  if (dateLocale.toLowerCase().startsWith("zh")) {
    return document.status === "draft"
      ? `草案更新于 ${date}。尚未生效：待批准和发布。`
      : `生效并最近更新于 ${date}。`;
  }
  return document.status === "draft"
    ? `Draft updated ${date}. Not yet in effect: pending approval and publication.`
    : `Effective and last updated ${date}`;
}
