import { InstallCodeBlock } from "@/components/install-code-block";
import { aptKeyFileName, type DesktopArch, type DesktopDownload, type LinuxDownloads } from "@/lib/desktop-release";
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

const KEYRING_DIR = "/etc/apt/keyrings";

export function aptInstallLines(apt: NonNullable<LinuxDownloads["apt"]>): string {
  const keyFile = aptKeyFileName(apt.keyUrl);
  if (!keyFile) throw new Error("apt key URL has no safe file name");
  const keyPath = `${KEYRING_DIR}/${keyFile}`;
  return [
    `sudo install -d -m 0755 ${KEYRING_DIR}`,
    `curl -fsSL ${apt.keyUrl} | sudo tee ${keyPath} >/dev/null`,
    `echo "deb [arch=${apt.architecture} signed-by=${keyPath}] ${apt.repository} ${apt.suite} ${apt.component}" | sudo tee /etc/apt/sources.list.d/codewhale.list >/dev/null`,
    `sudo apt update && sudo apt install ${apt.package}`,
  ].join("\n");
}

export function groupFingerprint(fingerprint: string): string {
  return fingerprint.match(/.{1,4}/g)?.join(" ") ?? fingerprint;
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
          <p>
            {chrome.downloadAptFingerprint.split("{fingerprint}").flatMap((part, index) =>
              index === 0 ? [part] : [<code key="fingerprint">{groupFingerprint(apt.keyFingerprint)}</code>, part],
            )}
          </p>
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
