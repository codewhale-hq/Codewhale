export type LegalDocumentId = "terms" | "privacy" | "membership";
export type LegalDocument = Readonly<{
  version: string;
  status: "draft" | "effective";
  effectiveAt: string | null;
}>;
export const LEGAL_DOCUMENTS: Readonly<Record<LegalDocumentId, LegalDocument>>;
export function formatLegalDocumentStatus(documentId: LegalDocumentId, dateLocale?: string): string;
