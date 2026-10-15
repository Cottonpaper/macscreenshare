<#
.SYNOPSIS
  Let the macscreenshare client receive the host's UDP video through Windows
  Firewall.

.DESCRIPTION
  The host streams video to the client over UDP. Windows Firewall blocks inbound
  UDP by default, and unlike a TCP listener the Windows Defender prompt often
  does not appear for a UDP socket — so the client connects, mouse/keyboard work
  (that is outbound TCP), but the window stays black.

  This adds an inbound allow rule scoped to the client executable (all profiles,
  so it works even when the network is marked Public). Run it once per build.

.PARAMETER ExePath
  One or more client executables to allow. Defaults to the debug and release
  binaries under client\target.

.PARAMETER SelfElevate
  Relaunch through UAC if not already running as administrator.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\windows-allow-firewall.ps1 -SelfElevate

.EXAMPLE
  .\scripts\windows-allow-firewall.ps1 -ExePath .\client\target\release\macss-client.exe
#>
[CmdletBinding()]
param(
    [string[]]$ExePath,
    [switch]$SelfElevate
)

$ErrorActionPreference = 'Stop'

function Test-Admin {
    ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
}

if ($SelfElevate -and -not (Test-Admin)) {
    $relaunch = @('-ExecutionPolicy', 'Bypass', '-File', $PSCommandPath, '-SelfElevate')
    if ($ExePath) { $relaunch += @('-ExePath', ($ExePath -join ',')) }
    Start-Process -Verb RunAs -FilePath 'powershell' -ArgumentList $relaunch
    return
}

if (-not (Test-Admin)) {
    Write-Error 'Adding a firewall rule needs administrator. Re-run with -SelfElevate to get a UAC prompt.'
    exit 1
}

if (-not $ExePath) {
    $root = Split-Path -Parent $PSScriptRoot
    $ExePath = @(
        (Join-Path $root 'client\target\debug\macss-client.exe'),
        (Join-Path $root 'client\target\release\macss-client.exe')
    )
}

$added = 0
foreach ($candidate in $ExePath) {
    if (-not (Test-Path -LiteralPath $candidate)) { continue }
    $full = (Resolve-Path -LiteralPath $candidate).Path
    $name = "macss-client inbound video ($full)"
    Get-NetFirewallRule -DisplayName $name -ErrorAction SilentlyContinue | Remove-NetFirewallRule
    New-NetFirewallRule -DisplayName $name -Direction Inbound -Action Allow -Protocol UDP `
        -Program $full -Profile Any | Out-Null
    Write-Host "Allowed inbound UDP for $full"
    $added++
}

if ($added -eq 0) {
    Write-Warning 'No client executable found. Build it first (cargo build -p macss-client), or pass -ExePath.'
}
