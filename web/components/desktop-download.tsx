import { InstallCodeBlock } from "@/components/install-code-block";
import type { DesktopArch, DesktopDownload, LinuxDownloads } from "@/lib/desktop-release";
import { fill, getChrome, getHome } from "@/lib/i18n/dictionaries";

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

const KEYRING = "/usr/share/keyrings/codewhale.gpg";

export function aptInstallLines(apt: NonNullable<LinuxDownloads["apt"]>): string {
  return [
    `curl -fsSL ${apt.keyUrl} | sudo gpg --dearmor -o ${KEYRING}`,
    `echo "deb [signed-by=${KEYRING}] ${apt.repository} ${apt.suite} ${apt.component}" | sudo tee /etc/apt/sources.list.d/codewhale.list`,
    `sudo apt update && sudo apt install ${apt.package}`,
  ].join("\n");
}

export function LinuxDownloadBlock({ linux, locale }: { linux: LinuxDownloads; locale: string }) {
  const chrome = getChrome(locale);
  const home = getHome(locale);
  const megabytes = new Intl.NumberFormat(chrome.dateLocale, { maximumFractionDigits: 1 });
  const { apt, appImage } = linux;
  return (
    <div className="desktop-download desktop-download-linux">
      <h3>{chrome.downloadLinux} · x64</h3>
      {apt ? (
        <>
          <p>{chrome.downloadAptTitle}</p>
          <InstallCodeBlock cmd={aptInstallLines(apt)} copyLabel={home.copy} copiedLabel={home.copied} copyLocale={locale} />
          <p>{chrome.downloadAptTrust}</p>
        </>
      ) : null}
      {appImage ? (
        <>
          <p>
            <a href={appImage.url} className="btn btn-secondary" rel="noopener">
              {chrome.downloadAppImage}
            </a>
          </p>
          <p>
            {fill(chrome.downloadVersion, { version: linux.version })} · {fill(chrome.downloadSize, { size: `${megabytes.format(appImage.size / 1_000_000)} MB` })}
          </p>
          <p>
            {chrome.downloadChecksum}: <code>{appImage.sha256}</code>
          </p>
          <p>{chrome.downloadAppImageTrust}</p>
        </>
      ) : null}
    </div>
  );
}
