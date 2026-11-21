param(
    [Parameter(Mandatory=$true)][string]$BaselineArchive,
    [Parameter(Mandatory=$true)][string]$BaselineArchiveDigest,
    [Parameter(Mandatory=$true)][string]$CandidateArchive,
    [Parameter(Mandatory=$true)][string]$CandidateArchiveDigest,
    [Parameter(Mandatory=$true)][string]$CandidateSource,
    [Parameter(Mandatory=$true)][string]$CandidateExecutableDigest
)
$ErrorActionPreference = 'Stop'
if ((Get-FileHash -LiteralPath $BaselineArchive).Hash.ToLowerInvariant() -cne $BaselineArchiveDigest) { throw 'Baseline archive checksum mismatch' }
if ((Get-FileHash -LiteralPath $CandidateArchive).Hash.ToLowerInvariant() -cne $CandidateArchiveDigest) { throw 'Candidate archive checksum mismatch' }
$root = Join-Path (Split-Path -Parent $CandidateArchive) ('archive-qualification-' + [guid]::NewGuid().ToString('N'))
$env:USERPROFILE = Join-Path $root 'profile'
$env:CODEWHALE_HOME = Join-Path $root 'state'
$env:CODEWHALE_NO_UPDATE_CHECK = '1'
$env:CODEWHALE_TELEMETRY = '0'
New-Item -ItemType Directory -Path $env:USERPROFILE,$env:CODEWHALE_HOME -Force | Out-Null
$originalUserPath = [Environment]::GetEnvironmentVariable('Path','User')
$bin = Join-Path $env:USERPROFILE 'bin'
$executables = @('codewhale.exe','codew.exe')
$ownedFiles = $executables + @('codewhale.bat','.codewhale-install.sha256')
$baseline = Join-Path $root 'baseline'
$candidate = Join-Path $root 'candidate'
Expand-Archive -LiteralPath $BaselineArchive -DestinationPath $baseline
Expand-Archive -LiteralPath $CandidateArchive -DestinationPath $candidate
$baselineInstaller = Join-Path $baseline "codewhale-$env:TARGET/install.ps1"
$candidateInstaller = Join-Path $candidate "codewhale-$env:TARGET/install.ps1"

foreach ($file in @('LICENSE','THIRD_PARTY_NOTICES.md','docs/THIRD_PARTY_NOTICES.md')) {
    $notice = Join-Path (Split-Path $candidateInstaller) "codewhale-notices/$file"
    if ((Get-FileHash $notice).Hash -cne (Get-FileHash (Join-Path 'product' $file)).Hash) { throw "Archive notice mismatch $file" }
}
function Verify-Version([string]$Expected, [string]$Digest) {
    foreach ($name in $executables) {
        $path = Join-Path $bin $name
        $reported = & $path --version
        if ($LASTEXITCODE -ne 0 -or $reported -cne $Expected) { throw "$name reported $reported; expected $Expected" }
        if ($Digest -and (Get-FileHash -LiteralPath $path).Hash.ToLowerInvariant() -cne $Digest) { throw "$name digest mismatch" }
    }
}

& $baselineInstaller
Verify-Version 'codewhale 0.10.2 (ff466b2ed0bf)' ''
'unrelated' | Set-Content (Join-Path $bin 'unrelated.txt')
'keep state' | Set-Content (Join-Path $env:CODEWHALE_HOME 'state.txt')
& $candidateInstaller -NoModifyPath
$version = "codewhale 0.10.3 ($($CandidateSource.Substring(0,12)))"
Verify-Version $version $CandidateExecutableDigest
$executable = Join-Path $bin 'codewhale.exe'
$receipt = Join-Path $bin '.codewhale-install.sha256'
if (!(Test-Path -LiteralPath $receipt)) { throw 'Missing archive ownership receipt' }
& $executable uninstall
if ($LASTEXITCODE -ne 0 -or !(Test-Path -LiteralPath $receipt)) { throw 'Uninstall preview changed ownership or failed' }
Verify-Version $version $CandidateExecutableDigest
& $executable uninstall --yes
if ($LASTEXITCODE -ne 0) { throw "Uninstall exited $LASTEXITCODE" }
foreach ($name in $ownedFiles) {
    if (Test-Path -LiteralPath (Join-Path $bin $name)) { throw "Uninstall retained $name" }
}
$deadline = [DateTime]::UtcNow.AddSeconds(15)
do {
    $recovery = @(Get-ChildItem -LiteralPath $bin -Force -Filter '.codewhale-host-recovery-*')
    if (!$recovery.Count) { break }
    Start-Sleep -Milliseconds 100
} while ([DateTime]::UtcNow -lt $deadline)
if ($recovery.Count) { throw 'Windows uninstall helper retained its recovery executable' }
if ((Get-Content (Join-Path $bin 'unrelated.txt')) -cne 'unrelated') { throw 'Uninstall changed unrelated data' }
if ((Get-Content (Join-Path $env:CODEWHALE_HOME 'state.txt')) -cne 'keep state') { throw 'Uninstall changed product state' }
if ([Environment]::GetEnvironmentVariable('Path','User') -cne $originalUserPath) { throw 'Archive test changed the user PATH' }
[ordered]@{source=$CandidateSource; rc_run=$env:RC_RUN; architecture=$env:RUNNER_ARCH; three_notices_match=$true; archive_sha256=$CandidateArchiveDigest; executable_sha256=$CandidateExecutableDigest; version=$version; build=[Environment]::OSVersion.Version.Build; baseline_install=$true; archive_upgrade=$true; uninstall_preview=$true; archive_uninstall=$true; user_path_unchanged=$true; state_preserved=$true} | ConvertTo-Json -Compress
