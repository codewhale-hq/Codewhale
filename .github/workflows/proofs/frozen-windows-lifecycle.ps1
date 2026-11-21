param(
    [Parameter(Mandatory=$true)][string]$Executable,
    [Parameter(Mandatory=$true)][string]$Source,
    [Parameter(Mandatory=$true)][string]$Digest
)
$ErrorActionPreference = 'Stop'
if ($Source -notmatch '^[a-f0-9]{40}$' -or $Digest -notmatch '^[a-f0-9]{64}$') { throw 'Invalid candidate identity' }
if ((Get-FileHash -Algorithm SHA256 -LiteralPath $Executable).Hash.ToLowerInvariant() -ne $Digest) { throw 'Candidate digest mismatch' }
$version = & $Executable --version
if ($LASTEXITCODE -ne 0 -or $version -notlike "*$($Source.Substring(0, 12))*") { throw 'Invalid candidate version stamp' }
$root = Join-Path (Split-Path -Parent $Executable) ('qualification-' + [guid]::NewGuid().ToString('N'))
$workspace = Join-Path $root 'work'
$homePath = Join-Path $root 'home'
New-Item -ItemType Directory -Path $workspace,$homePath -Force | Out-Null
$env:CODEWHALE_HOME = $homePath
$env:CODEWHALE_TELEMETRY = '0'
$env:CODEWHALE_DISABLE_MODELS_DEV_FETCH = '1'
$env:CODEWHALE_RUNTIME_TOKEN = [guid]::NewGuid().ToString('N')
$headers = @{Authorization = "Bearer $env:CODEWHALE_RUNTIME_TOKEN"}
@'
param([string]$Marker)
$PID | Set-Content -LiteralPath $Marker
Start-Sleep -Seconds 90
'@ | Set-Content -LiteralPath (Join-Path $workspace 'worker.ps1')
'preserved' | Set-Content -LiteralPath (Join-Path $workspace 'preserved.txt')

function Invoke-Api($Method, $Path, $Payload) {
    $parameters = @{Uri="$script:baseUrl$Path"; Headers=$headers; Method=$Method; TimeoutSec=10; UseBasicParsing=$true}
    if ($null -ne $Payload) {
        $parameters.Body = $Payload | ConvertTo-Json -Compress
        $parameters.ContentType = 'application/json'
    }
    Invoke-RestMethod @parameters
}

function Start-Server($Label) {
    $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $listener.Start()
    try { $port = $listener.LocalEndpoint.Port } finally { $listener.Stop() }
    $script:baseUrl = "http://127.0.0.1:$port"
    $script:server = Start-Process -FilePath $Executable -ArgumentList @('app-server','--http','--port',"$port",'--workers','1') -WorkingDirectory $workspace -RedirectStandardOutput (Join-Path $root "$Label.out") -RedirectStandardError (Join-Path $root "$Label.err") -PassThru
    $deadline = [DateTime]::UtcNow.AddSeconds(25)
    do {
        if ($script:server.HasExited) { throw "Engine exited during startup. Logs in $root" }
        try {
            $info = Invoke-Api Get '/v1/runtime/info'
            if ($info.codewhale_commit -eq $Source -and $info.capabilities.job_operations -and $info.bind_host -eq '127.0.0.1') { return }
        } catch {
            Start-Sleep -Milliseconds 200
        }
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Engine startup did not report the expected source and capabilities. Logs in $root"
}

function Start-OwnedJob($Thread, $Marker) {
    $command = "powershell.exe -NoProfile -ExecutionPolicy Bypass -File worker.ps1 $Marker"
    $reply = Invoke-Api Post "/v1/threads/$Thread/jobs" @{command=$command}
    $markerPath = Join-Path $workspace $Marker
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    do {
        if (Test-Path -LiteralPath $markerPath) {
            $processId = Get-Content -LiteralPath $markerPath | Select-Object -First 1
            if ($processId -match '^\d+$') {
                $child = Get-Process -Id ([int]$processId) -ErrorAction SilentlyContinue
                if ($null -ne $child) {
                    $script:children += $child
                    return @{job=$reply.job.id; process=$child}
                }
            }
        }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Did not observe owned job process for $Marker. Logs in $root"
}

$script:server = $null
$script:children = @()
try {
    Start-Server 'initial'
    $first = (Invoke-Api Post '/v1/threads' @{workspace=$workspace}).id
    $second = (Invoke-Api Post '/v1/threads' @{workspace=$workspace}).id
    if (!$first -or !$second -or $first -eq $second) { throw 'Expected distinct owned threads' }
    $deletedJob = Start-OwnedJob $first 'deleted.pid'
    $crashJob = Start-OwnedJob $second 'crashed.pid'
    $null = Invoke-Api Delete "/v1/threads/$first"
    if (!$deletedJob.process.WaitForExit(10000)) { throw 'Deleted thread left its worker alive' }
    if ($crashJob.process.HasExited) { throw 'Thread deletion killed its sibling worker' }
    $server.Kill()
    if (!$server.WaitForExit(10000)) { throw 'Owned Engine process did not exit' }
    if (!$crashJob.process.WaitForExit(10000)) { throw 'Crashed Engine left its worker alive' }
    $crashMarkerHash = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $workspace 'crashed.pid')).Hash
    Start-Server 'reopened'
    $null = Invoke-Api Get "/v1/threads/$second"
    if (@((Invoke-Api Get '/v1/jobs').jobs).Count -ne 0) { throw 'Reopen projected a live job' }
    if ((Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $workspace 'crashed.pid')).Hash -ne $crashMarkerHash) { throw 'Reopen reran the worker' }
    if ((Get-Content -LiteralPath (Join-Path $workspace 'preserved.txt')) -ne 'preserved') { throw 'Workspace sentinel changed' }
    [ordered]@{source=$Source; sha256=$Digest; version=$version; native_windows_build=[Environment]::OSVersion.Version.Build; workspace=$root; health=$true; delete_cleanup=$true; sibling_survived=$true; owner_crash_cleanup=$true; reopen=$true; no_rerun=$true; workspace_preserved=$true} | ConvertTo-Json -Compress
} finally {
    if ($null -ne $server -and !$server.HasExited) { $server.Kill(); $null = $server.WaitForExit(10000) }
    foreach ($child in $children) { if (!$child.HasExited) { $child.Kill(); $null = $child.WaitForExit(5000) } }
}
