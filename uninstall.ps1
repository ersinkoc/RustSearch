#requires -Version 5.1
[CmdletBinding()]
param()
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$marker = Get-Content -LiteralPath (Join-Path $PSScriptRoot '.rustsearch-install.json') -Raw | ConvertFrom-Json
$options = @{
    Uninstall = $true
    InstallDirectory = $PSScriptRoot
    DataDirectory = $marker.data_directory
    NoIntegration = (-not $marker.integration)
}
& (Join-Path $PSScriptRoot 'install.ps1') @options
