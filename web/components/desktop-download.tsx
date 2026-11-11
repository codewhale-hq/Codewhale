import type { DesktopDownload } from "@/lib/desktop-release";
import { fill, getChrome } from "@/lib/i18n/dictionaries";

export function DesktopDownloadBlock({ download, locale }: { download: DesktopDownload; locale: string }) {
  const chrome = getChrome(locale);
  const size = `${new Intl.NumberFormat(chrome.dateLocale, { maximumFractionDigits: 1 }).format(download.size / 1_000_000)} MB`;
  return (
    <div className="desktop-download">
      <p>
        <a href={download.url} className="btn btn-primary" rel="noopener">
          {chrome.downloadMacos}
        </a>
      </p>
      <p>
        {fill(chrome.downloadVersion, { version: download.version })} · {fill(chrome.downloadSize, { size })} · {chrome.downloadSigned}
      </p>
      <p>
        {chrome.downloadChecksum}: <code>{download.sha256}</code>
      </p>
    </div>
  );
}
