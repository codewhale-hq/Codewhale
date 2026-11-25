import type { DesktopArch, DesktopDownload } from "@/lib/desktop-release";
import { fill, getChrome } from "@/lib/i18n/dictionaries";

const ARCH_NAME: Record<DesktopArch, string> = { arm64: "Apple silicon", x64: "Intel" };

export function DesktopDownloadBlock({ downloads, locale }: { downloads: DesktopDownload[]; locale: string }) {
  const chrome = getChrome(locale);
  const megabytes = new Intl.NumberFormat(chrome.dateLocale, { maximumFractionDigits: 1 });
  const named = downloads.length > 1;
  return (
    <>
      {downloads.map((download) => {
        const size = `${megabytes.format(download.size / 1_000_000)} MB`;
        const arch = named && download.arch ? ARCH_NAME[download.arch] : null;
        return (
          <div className="desktop-download" key={download.url}>
            <p>
              <a href={download.url} className="btn btn-primary" rel="noopener">
                {chrome.downloadMacos}
                {arch ? ` · ${arch}` : null}
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
      })}
    </>
  );
}
