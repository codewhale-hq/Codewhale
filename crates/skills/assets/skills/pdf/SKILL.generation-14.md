---
name: pdf
description: Read, extract, split, merge, rotate, watermark, fill, OCR, or create PDF files with verification of page counts and text extraction.
---

# PDF

Use this skill for any task where a PDF is the primary input or output.

## Workflow

1. Identify the PDF operation: read, extract, OCR, split, merge, rotate,
   watermark, redact, fill forms, encrypt/decrypt, or create.
2. Preserve originals. Write outputs with explicit names.
3. Use the most reliable available tool:
   - the built-in `File` tool (`action: "read"`) for basic text extraction from PDFs
   - `pdftotext`, `pdfinfo`, `qpdf`, or `mutool` when installed
   - Python libraries such as `pypdf`, `pdfplumber`, `PyMuPDF`, or
     `reportlab` when available
   - OCR tools only for scanned pages
4. For extraction, report page coverage and note when layout, tables, or OCR
   quality may affect accuracy.
5. For generated or modified PDFs, verify page count, text extraction where
   possible, and file size. For redaction, confirm removed text is not
   extractable from the output.

Ask before installing dependencies or running OCR over large documents. Do not
represent a visually scanned PDF as fully accurate text unless OCR quality has
been checked.
