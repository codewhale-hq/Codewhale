[CmdletBinding()]
param(
    [ValidateSet('Add', 'Remove')]
    [string] $Operation,

    [string] $Entry
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-NormalizedPathEntry {
    param(
        [AllowNull()]
        [AllowEmptyString()]
        [string] $Value
    )

    if ($null -eq $Value) {
        return ''
    }

    $normalized = $Value.Trim()
    if (
        $normalized.Length -ge 2 -and
        $normalized[0] -eq '"' -and
        $normalized[$normalized.Length - 1] -eq '"'
    ) {
        $normalized = $normalized.Substring(1, $normalized.Length - 2).Trim()
    }

    return $normalized.TrimEnd([char[]] @('\', '/'))
}

function Test-PathEntryEqual {
    param(
        [AllowNull()]
        [AllowEmptyString()]
        [string] $Left,

        [AllowNull()]
        [AllowEmptyString()]
        [string] $Right
    )

    $normalizedLeft = Get-NormalizedPathEntry -Value $Left
    $normalizedRight = Get-NormalizedPathEntry -Value $Right

    if ($normalizedLeft.Length -eq 0 -or $normalizedRight.Length -eq 0) {
        return $false
    }

    return [string]::Equals(
        $normalizedLeft,
        $normalizedRight,
        [System.StringComparison]::OrdinalIgnoreCase
    )
}

function Get-UpdatedUserPath {
    param(
        [AllowNull()]
        [AllowEmptyString()]
        [string] $Current,

        [Parameter(Mandatory = $true)]
        [ValidateSet('Add', 'Remove')]
        [string] $RequestedOperation,

        [Parameter(Mandatory = $true)]
        [string] $RequestedEntry
    )

    if ([string]::IsNullOrWhiteSpace((Get-NormalizedPathEntry -Value $RequestedEntry))) {
        throw 'The PATH entry must not be empty.'
    }

    if ($null -eq $Current) {
        $Current = ''
    }

    $parts = $Current.Split([char[]] @(';'), [System.StringSplitOptions]::None)
    $containsEntry = $false
    foreach ($part in $parts) {
        if (Test-PathEntryEqual -Left $part -Right $RequestedEntry) {
            $containsEntry = $true
            break
        }
    }

    if ($RequestedOperation -eq 'Add') {
        if ($containsEntry) {
            return $Current
        }
        if ($Current.Length -eq 0) {
            return $RequestedEntry
        }
        if ($Current.EndsWith(';')) {
            return "$Current$RequestedEntry;"
        }
        return "$Current;$RequestedEntry"
    }

    if (-not $containsEntry) {
        return $Current
    }

    $remaining = New-Object 'System.Collections.Generic.List[string]'
    foreach ($part in $parts) {
        if (-not (Test-PathEntryEqual -Left $part -Right $RequestedEntry)) {
            [void] $remaining.Add($part)
        }
    }

    return [string]::Join(';', $remaining)
}

function Set-RegistryPathEntry {
    param(
        [Parameter(Mandatory = $true)]
        [Microsoft.Win32.RegistryKey] $RegistryKey,

        [Parameter(Mandatory = $true)]
        [ValidateSet('Add', 'Remove')]
        [string] $RequestedOperation,

        [Parameter(Mandatory = $true)]
        [string] $RequestedEntry
    )

    $pathExists = @($RegistryKey.GetValueNames()) -contains 'Path'
    if ($pathExists) {
        $valueKind = $RegistryKey.GetValueKind('Path')
        if (
            $valueKind -ne [Microsoft.Win32.RegistryValueKind]::String -and
            $valueKind -ne [Microsoft.Win32.RegistryValueKind]::ExpandString
        ) {
            throw "The user PATH has unsupported registry type '$valueKind'."
        }

        $current = [string] $RegistryKey.GetValue(
            'Path',
            $null,
            [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames
        )
        if ($null -eq $current) {
            $current = ''
        }
    }
    else {
        $current = ''
        $valueKind = [Microsoft.Win32.RegistryValueKind]::ExpandString
    }

    $updated = Get-UpdatedUserPath `
        -Current $current `
        -RequestedOperation $RequestedOperation `
        -RequestedEntry $RequestedEntry

    if ([string]::Equals($current, $updated, [System.StringComparison]::Ordinal)) {
        return $false
    }

    $RegistryKey.SetValue('Path', $updated, $valueKind)
    return $true
}

function Set-CodeWhaleUserPath {
    param(
        [Parameter(Mandatory = $true)]
        [ValidateSet('Add', 'Remove')]
        [string] $RequestedOperation,

        [Parameter(Mandatory = $true)]
        [string] $RequestedEntry
    )

    if ([System.Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
        throw 'The CodeWhale user PATH helper only supports Windows.'
    }

    $environmentKey = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment')
    if ($null -eq $environmentKey) {
        throw 'Could not open or create the current-user Environment registry key.'
    }

    try {
        return Set-RegistryPathEntry `
            -RegistryKey $environmentKey `
            -RequestedOperation $RequestedOperation `
            -RequestedEntry $RequestedEntry
    }
    finally {
        $environmentKey.Close()
    }
}

if ($MyInvocation.InvocationName -ne '.') {
    if ([string]::IsNullOrWhiteSpace($Operation)) {
        throw 'Operation is required.'
    }
    if ([string]::IsNullOrWhiteSpace($Entry)) {
        throw 'Entry is required.'
    }

    $changed = Set-CodeWhaleUserPath `
        -RequestedOperation $Operation `
        -RequestedEntry $Entry

    if ($changed) {
        Write-Host "User PATH updated: $Operation '$Entry'."
    }
    else {
        Write-Host "User PATH already satisfied: $Operation '$Entry'."
    }
}
