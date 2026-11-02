# WinGet packaging for CodeWhale

WinGet installs `HunterBown.CodeWhale` as the portable x64 command `codewhale`. It uses the legacy asset filename `codewhale-tui-windows-x64.exe` and declares the Microsoft Visual C++ 2015+ x64 runtime dependency. The published 0.10.2 package uses this contract. The release also provides a separate NSIS installer and native ARM64 archives. [Microsoft’s 0.10.2 manifest](https://github.com/microsoft/winget-pkgs/tree/master/manifests/h/HunterBown/CodeWhale/0.10.2)

## Prepare a manifest

Run the generator with the release version and its checksum directory. It selects the exact portable executable from `codewhale-artifacts-sha256.txt`, updates `packaging/winget/HunterBown.CodeWhale.yaml`, and copies the same bytes into `.winget/`.

```bash
./packaging/winget/generate-winget-manifest.sh X.Y.Z /path/to/release-assets
```

Validate the resulting singleton on Windows.

```powershell
winget validate --manifest packaging/winget/HunterBown.CodeWhale.yaml
```

Microsoft maintains the public package as a multi-file manifest. A separately approved submission updates that package using the verified installer URL, hash, architecture and command. [Published manifests](https://github.com/microsoft/winget-pkgs/tree/master/manifests/h/HunterBown/CodeWhale)

## FreeBSD

FreeBSD has no prebuilt GitHub Release asset (see `docs/INSTALL.md` § FreeBSD). Install via Cargo:

```bash
pkg install -y rust pkgconf  # or ports-mgmt/pkg
cargo install codewhale-cli --locked   # provides `codewhale`
```

The npm wrapper on FreeBSD exits with `Unsupported platform: freebsd` and points to the Cargo path.
A native `pkg install codewhale` port is tracked as a follow-up to #1097 — contributions welcome
under `packaging/freebsd/`.
