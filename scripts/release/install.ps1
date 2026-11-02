# Archive installation only. Runtime authority remains with Codewhale Core.
$ErrorActionPreference = 'Stop'
$destination = Join-Path $env:USERPROFILE 'bin'
$names = @('codewhale.exe', 'codew.exe', 'codewhale.bat')
$optional = $env:CODEWHALE_INSTALL_COMPILED_HOST -eq '1'
if ($optional) {
    $names += @('codewhale-extension-host.exe', 'codewhale-extension-host.LICENSES.txt', 'codewhale-extension-host.relink-source.tar.gz', 'codewhale-extension-host.release.json')
}
function File-Hash([string]$file) {
    if (!(Test-Path -LiteralPath $file)) { return $null }
    $item = Get-Item -LiteralPath $file -Force
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw "Refusing nonregular installation file $file" }
    return (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLowerInvariant()
}
function Host-Entry([string]$receipt) {
    if ((Get-Item -LiteralPath $receipt).Length -gt 65536) { throw 'Compiled-host receipt exceeds 64 KiB' }
    $catalog = Get-Content -LiteralPath $receipt -Raw | ConvertFrom-Json
    $architecture = [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLowerInvariant()
    if ($architecture -eq 'x64') { $target = 'windows-x64' }
    elseif ($architecture -eq 'arm64') { $target = 'windows-arm64' }
    else { throw "No qualified compiled host for Windows/$architecture; use Node" }
    $rows = @($catalog.hosts | Where-Object { $_.target -eq $target })
    if ($catalog.schema -ne 1 -or $rows.Count -ne 1) { throw 'No unique qualified compiled-host receipt' }
    $hostEntry = $rows[0]
    if ($hostEntry.license_closure -ne 'complete' -or $hostEntry.relink_source -ne 'complete' -or $hostEntry.native_platform -ne 'win32' -or $hostEntry.native_arch -ne $architecture -or $hostEntry.passed -ne 5 -or $hostEntry.failed -ne 0 -or $hostEntry.skipped -ne 0 -or $hostEntry.native_passed -lt 9 -or $hostEntry.native_failed -ne 0 -or $hostEntry.native_skipped -ne 0) { throw 'Compiled host has no complete matching-native qualification' }
    return $hostEntry
}
$stage = $null
$published = @()
$retain = $false
try {
    # Check every source before the first destination mutation.
    foreach ($name in $names) {
        if (!(File-Hash (Join-Path $PSScriptRoot $name))) { throw "Missing archive payload $name; extract the complete archive" }
    }
    if ((File-Hash (Join-Path $PSScriptRoot 'codewhale.exe')) -ne (File-Hash (Join-Path $PSScriptRoot 'codew.exe'))) { throw 'Command alias differs from the canonical Codewhale program' }
    if ($optional) {
        $entry = Host-Entry (Join-Path $PSScriptRoot 'codewhale-extension-host.release.json')
        $hashes = @($entry.sha256, $entry.notices_sha256, $entry.source_sha256)
        for ($i = 0; $i -lt 3; $i++) {
            if ($hashes[$i] -notmatch '^[a-f0-9]{64}$' -or (File-Hash (Join-Path $PSScriptRoot $names[$i + 3])) -ne $hashes[$i]) { throw 'Compiled host archive payload hash mismatch' }
        }
    }
    if (Test-Path -LiteralPath $destination) {
        $directory = Get-Item -LiteralPath $destination -Force
        if (!$directory.PSIsContainer -or ($directory.Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'Refusing a nonregular or redirected installation directory' }
    }
    [IO.Directory]::CreateDirectory($destination) | Out-Null
    $stage = Join-Path $destination ('.codewhale-install-' + [Guid]::NewGuid().ToString('N'))
    [IO.Directory]::CreateDirectory($stage) | Out-Null
    $oldHost = $null
    if ($optional -and (File-Hash (Join-Path $destination 'codewhale-extension-host.release.json'))) { $oldHost = Host-Entry (Join-Path $destination 'codewhale-extension-host.release.json') }
    $snapshots = @()
    foreach ($name in $names) {
        $target = Join-Path $destination $name
        $oldHash = File-Hash $target
        if ($optional -and $name -like 'codewhale-extension-host*' -and $name -ne 'codewhale-extension-host.release.json' -and $oldHash) {
            if (!$oldHost) { throw "Refusing unclaimed compiled-host destination $target" }
            $expected = switch ($name) { 'codewhale-extension-host.exe' { $oldHost.sha256 }; 'codewhale-extension-host.LICENSES.txt' { $oldHost.notices_sha256 }; 'codewhale-extension-host.relink-source.tar.gz' { $oldHost.source_sha256 } }
            if ($oldHash -ne $expected) { throw "Refusing modified compiled-host destination $target" }
        }
        $staged = Join-Path $stage $name
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot $name) -Destination $staged
        $newHash = File-Hash $staged
        if ($newHash -ne (File-Hash (Join-Path $PSScriptRoot $name))) { throw "Source changed during staging: $name" }
        $backup = Join-Path $stage ($name + '.previous')
        if ($oldHash) {
            Copy-Item -LiteralPath $target -Destination $backup
            if ((File-Hash $backup) -ne $oldHash) { throw "Destination changed during backup: $target" }
        }
        $snapshots += [PSCustomObject]@{ Name=$name; Target=$target; Staged=$staged; OldHash=$oldHash; NewHash=$newHash; Backup=$backup }
    }
    $receiptName = '.codewhale-install.sha256'
    $receiptStaged = Join-Path $stage $receiptName
    $receiptText = ($snapshots | Where-Object { $_.Name -in @('codewhale.exe', 'codew.exe', 'codewhale.bat') } | ForEach-Object { "$($_.NewHash)  $($_.Name)`n" }) -join ''
    [IO.File]::WriteAllText($receiptStaged, $receiptText)
    $receiptTarget = Join-Path $destination $receiptName
    $receiptOld = File-Hash $receiptTarget
    $receiptBackup = Join-Path $stage ($receiptName + '.previous')
    if ($receiptOld) {
        Copy-Item -LiteralPath $receiptTarget -Destination $receiptBackup
        if ((File-Hash $receiptBackup) -ne $receiptOld) { throw "Destination changed during backup: $receiptTarget" }
    }
    $snapshots += [PSCustomObject]@{ Name=$receiptName; Target=$receiptTarget; Staged=$receiptStaged; OldHash=$receiptOld; NewHash=(File-Hash $receiptStaged); Backup=$receiptBackup }
    foreach ($row in $snapshots) { if ((File-Hash $row.Target) -ne $row.OldHash) { throw "Destination changed before publication: $($row.Target)" } }
    foreach ($row in $snapshots) {
        if ((File-Hash $row.Target) -ne $row.OldHash) { throw "Destination changed during publication: $($row.Target)" }
        Move-Item -LiteralPath $row.Staged -Destination $row.Target -Force:([bool]$row.OldHash)
        $published += $row
    }
    Write-Host "Installed canonical Codewhale commands to $destination. Add this directory to your user PATH. Node remains the default host. Remove later with: `"$destination\codewhale.exe`" uninstall"
    $kept = @($published | Where-Object { $_.OldHash -and $_.OldHash -ne $_.NewHash -and (Test-Path -LiteralPath $_.Backup) })
    if ($kept.Count -gt 0) {
        $dataRoot = if ($env:CODEWHALE_HOME -and [IO.Path]::IsPathRooted($env:CODEWHALE_HOME)) { $env:CODEWHALE_HOME } else { Join-Path $env:USERPROFILE '.codewhale' }
        $stamp = [DateTime]::UtcNow.ToString("yyyyMMdd'T'HHmmss'Z'")
        $recovery = Join-Path (Join-Path $dataRoot 'recovery') $stamp
        for ($attempt = 1; Test-Path -LiteralPath $recovery; $attempt++) { $recovery = Join-Path (Join-Path $dataRoot 'recovery') "$stamp-$attempt" }
        [IO.Directory]::CreateDirectory($recovery) | Out-Null
        $seq = 0
        foreach ($row in $kept) {
            $seq++
            $stored = Join-Path $recovery ('{0:D3}-{1}' -f $seq, $row.Name)
            Move-Item -LiteralPath $row.Backup -Destination $stored
            [IO.File]::AppendAllText((Join-Path $recovery 'manifest.tsv'), ("{0}`t{1}`t{2}`t{3}`n" -f $row.Target, $stored, $row.OldHash, (Get-Item -LiteralPath $stored).Length))
        }
        Write-Host "Upgraded the earlier Codewhale. Previous files are kept in $recovery for 30 days. Undo: `"$destination\codewhale.exe`" doctor --restore $(Split-Path -Leaf $recovery)"
    }
    try { & (Join-Path $destination 'codewhale.exe') doctor --installs --brief 2>$null } catch { }
    Write-Host "Updates install automatically; turn off with: codewhale config set update.mode notify"
} catch {
    $original = $_
    [array]::Reverse($published)
    foreach ($row in $published) {
        try {
            if ((File-Hash $row.Target) -ne $row.NewHash) { throw "Published file changed; preserved $($row.Target)" }
            if ($row.OldHash) { Move-Item -LiteralPath $row.Backup -Destination $row.Target -Force }
            else { Remove-Item -LiteralPath $row.Target }
        } catch { $retain = $true; Write-Warning $_ }
    }
    if ($retain) { Write-Warning "Rollback incomplete; recovery files retained at $stage" }
    Write-Error $original -ErrorAction Continue
    exit 1
} finally {
    if ($stage -and !$retain) { Remove-Item -LiteralPath $stage -Recurse -Force }
}
