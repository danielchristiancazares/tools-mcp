#!/usr/bin/env pwsh

<#
.SYNOPSIS
  Generate local HTML coverage reports using cargo-llvm-cov.

.DESCRIPTION
  Produces an HTML report in ./coverage/ (gitignored) and prints the path to the
  main index.html.

  Prereqs:
    - rustup + a Rust toolchain installed
    - rustup component: llvm-tools-preview
    - cargo subcommand: cargo-llvm-cov

  This script can install missing prerequisites with -Install.

.PARAMETER Install
  Installs missing prerequisites (llvm-tools-preview + cargo-llvm-cov).

.PARAMETER Open
  Opens the generated HTML report (coverage/index.html) after generating it.
#>

[CmdletBinding()]
param(
  [switch]$Install,
  [switch]$Open
)

$ErrorActionPreference = "Stop"

function Assert-Command {
  param([Parameter(Mandatory=$true)][string]$Name, [string]$Hint)
  if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
    Write-Error "Missing required command: $Name. $Hint"
  }
}

Assert-Command -Name "cargo" -Hint "Install Rust and ensure cargo is on PATH."

if (-not (Get-Command "rustup" -ErrorAction SilentlyContinue)) {
  if ($Install) {
    Write-Error "rustup is required to install llvm-tools-preview. Please install rustup first."
  } else {
    Write-Error "Missing required command: rustup. Install rustup or rerun with -Install after installing it."
  }
}

$installedComponents = & rustup component list --installed 2>$null
if (-not $installedComponents) {
  Write-Error "Failed to query rustup installed components. Is rustup configured correctly?"
}

if ($installedComponents -notmatch "^llvm-tools-preview") {
    if ($Install) {
        Write-Host "Installing rustup component: llvm-tools-preview"
        & rustup component add llvm-tools-preview | Write-Host
    } else {
    Write-Error "Missing rustup component llvm-tools-preview. Run: rustup component add llvm-tools-preview (or rerun this script with -Install)."
  }
}

try {
  & cargo llvm-cov --version *> $null
} catch {
  if ($Install) {
    Write-Host "Installing cargo subcommand: cargo-llvm-cov"
    & cargo install cargo-llvm-cov | Write-Host
  } else {
    Write-Error "Missing cargo subcommand cargo-llvm-cov. Run: cargo install cargo-llvm-cov (or rerun this script with -Install)."
  }
}

if (-not (Test-Path "coverage")) {
  New-Item -ItemType Directory -Path "coverage" | Out-Null
}

Write-Host "Generating HTML coverage report into ./coverage/"
& cargo llvm-cov --workspace --html --output-dir coverage

$index = Join-Path (Get-Location) "coverage\\index.html"
Write-Host "Coverage report: $index"

if ($Open) {
  Start-Process $index | Out-Null
}
