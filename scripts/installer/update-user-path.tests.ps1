$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

. "$PSScriptRoot\update-user-path.ps1"

function Assert-Equal {
    param(
        [AllowNull()]
        $Expected,

        [AllowNull()]
        $Actual,

        [Parameter(Mandatory = $true)]
        [string] $Because
    )

    if (-not [object]::Equals($Expected, $Actual)) {
        throw "$Because`nExpected: <$Expected>`nActual:   <$Actual>"
    }
}

$entry = 'C:\CodeWhalePathTest\bin'
$longPath = ((1..80) | ForEach-Object { 'C:\CWCanary\DevelopmentTool{0:D4}\bin' -f $_ }) -join ';'
if ($longPath.Length -le 1800) {
    throw "The long-PATH fixture is too short: $($longPath.Length)."
}

$addedToEmpty = Get-UpdatedUserPath `
    -Current '' `
    -RequestedOperation Add `
    -RequestedEntry $entry
Assert-Equal $entry $addedToEmpty 'Adding to a missing or empty PATH must create only the requested entry.'

$added = Get-UpdatedUserPath `
    -Current $longPath `
    -RequestedOperation Add `
    -RequestedEntry $entry
Assert-Equal "$longPath;$entry" $added 'Adding to a long PATH must preserve every existing character.'

$expanded = '%USERPROFILE%\bin;C:\Tools'
$expandedAdded = Get-UpdatedUserPath `
    -Current $expanded `
    -RequestedOperation Add `
    -RequestedEntry $entry
Assert-Equal "$expanded;$entry" $expandedAdded 'Expandable variable references must remain unexpanded.'

$alreadyPresent = "$longPath;$($entry.ToUpperInvariant())\"
$unchanged = Get-UpdatedUserPath `
    -Current $alreadyPresent `
    -RequestedOperation Add `
    -RequestedEntry $entry
Assert-Equal $alreadyPresent $unchanged 'Adding must be idempotent across case and a trailing slash.'

$substringPath = 'C:\CodeWhalePathTest\bin-tools;C:\Other'
$substringAdded = Get-UpdatedUserPath `
    -Current $substringPath `
    -RequestedOperation Add `
    -RequestedEntry $entry
Assert-Equal "$substringPath;$entry" $substringAdded 'A substring must not count as an exact PATH entry.'

$trailingDelimiter = 'C:\One;C:\Two;'
$trailingAdded = Get-UpdatedUserPath `
    -Current $trailingDelimiter `
    -RequestedOperation Add `
    -RequestedEntry $entry
Assert-Equal "$trailingDelimiter$entry;" $trailingAdded 'Adding must preserve the existing trailing delimiter.'

$removalSource = "C:\Before;$entry;C:\After"
$removed = Get-UpdatedUserPath `
    -Current $removalSource `
    -RequestedOperation Remove `
    -RequestedEntry $entry
Assert-Equal 'C:\Before;C:\After' $removed 'Removing must delete only the exact CodeWhale entry.'

$duplicateSource = "$entry;C:\Keep;$($entry.ToUpperInvariant())\"
$duplicatesRemoved = Get-UpdatedUserPath `
    -Current $duplicateSource `
    -RequestedOperation Remove `
    -RequestedEntry $entry
Assert-Equal 'C:\Keep' $duplicatesRemoved 'Uninstall must remove all equivalent CodeWhale entries.'

$unrelated = 'C:\One;C:\CodeWhalePathTest\bin-tools;C:\Two'
$unrelatedResult = Get-UpdatedUserPath `
    -Current $unrelated `
    -RequestedOperation Remove `
    -RequestedEntry $entry
Assert-Equal $unrelated $unrelatedResult 'Removing an absent entry must leave the PATH byte-for-byte unchanged.'

if ([System.Environment]::OSVersion.Platform -eq [System.PlatformID]::Win32NT) {
    $testKeyPath = "Software\CodeWhale\Tests\UserPath-$PID-$([guid]::NewGuid().ToString('N'))"
    $testKey = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($testKeyPath)
    if ($null -eq $testKey) {
        throw 'Could not create the isolated registry test key.'
    }

    try {
        $testKey.SetValue('Path', $longPath, [Microsoft.Win32.RegistryValueKind]::ExpandString)
        $changed = Set-RegistryPathEntry `
            -RegistryKey $testKey `
            -RequestedOperation Add `
            -RequestedEntry $entry
        Assert-Equal $true $changed 'The registry helper must report a long PATH update.'

        $raw = [string] $testKey.GetValue(
            'Path',
            $null,
            [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames
        )
        Assert-Equal "$longPath;$entry" $raw 'The registry helper must not truncate a long PATH.'
        Assert-Equal `
            ([Microsoft.Win32.RegistryValueKind]::ExpandString) `
            ($testKey.GetValueKind('Path')) `
            'The registry helper must preserve REG_EXPAND_SZ.'

        $changedAgain = Set-RegistryPathEntry `
            -RegistryKey $testKey `
            -RequestedOperation Add `
            -RequestedEntry $entry
        Assert-Equal $false $changedAgain 'A repeated install must not rewrite the registry value.'

        $removedFromRegistry = Set-RegistryPathEntry `
            -RegistryKey $testKey `
            -RequestedOperation Remove `
            -RequestedEntry $entry
        Assert-Equal $true $removedFromRegistry 'The registry helper must remove the installed entry.'

        $restored = [string] $testKey.GetValue(
            'Path',
            $null,
            [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames
        )
        Assert-Equal $longPath $restored 'Install followed by uninstall must restore the long PATH exactly.'

        $testKey.SetValue('Path', 'C:\Plain', [Microsoft.Win32.RegistryValueKind]::String)
        [void] (Set-RegistryPathEntry `
            -RegistryKey $testKey `
            -RequestedOperation Add `
            -RequestedEntry $entry)
        Assert-Equal `
            ([Microsoft.Win32.RegistryValueKind]::String) `
            ($testKey.GetValueKind('Path')) `
            'The registry helper must preserve REG_SZ.'

        $testKey.DeleteValue('Path')
        $missingRemoval = Set-RegistryPathEntry `
            -RegistryKey $testKey `
            -RequestedOperation Remove `
            -RequestedEntry $entry
        Assert-Equal $false $missingRemoval 'Removing from a missing PATH must be a no-op.'
        Assert-Equal `
            $false `
            (@($testKey.GetValueNames()) -contains 'Path') `
            'A no-op uninstall must not create a missing PATH value.'
    }
    finally {
        $testKey.Close()
        [Microsoft.Win32.Registry]::CurrentUser.DeleteSubKeyTree($testKeyPath, $false)
    }
}

Write-Host 'Windows installer PATH helper tests passed.'
