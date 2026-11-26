<#
.SYNOPSIS
    AgnoPeriph developer task runner (native Windows / PowerShell).
.EXAMPLE
    .\scripts\dev.ps1 check
    .\scripts\dev.ps1 test agnoperiph
    .\scripts\dev.ps1 visual triggers
    .\scripts\dev.ps1 visual-baseline
    .\scripts\dev.ps1 mock
    .\scripts\dev.ps1 watch-mock
    .\scripts\dev.ps1 prune
    .\scripts\dev.ps1 sweep 14
    .\scripts\dev.ps1 clean-dev
#>
param(
    [Parameter(Position = 0)]
    [ValidateSet("check", "test", "visual", "visual-baseline", "mock", "watch-mock", "prune", "clean-dev", "sweep", "help")]
    [string]$Task = "help",

    [Parameter(Position = 1)]
    [string]$Arg = ""
)

$ErrorActionPreference = "Stop"

switch ($Task) {
    "check" {
        cargo fmt --all -- --check
        cargo clippy --workspace --all-targets -- -D warnings
        cargo test --workspace
    }
    "test" {
        if ([string]::IsNullOrWhiteSpace($Arg)) {
            cargo test --workspace
        } else {
            cargo test -p $Arg
        }
    }
    "visual" {
        if (-not [string]::IsNullOrWhiteSpace($Arg)) {
            $env:AGNO_VISUAL = $Arg
        } else {
            Remove-Item Env:AGNO_VISUAL -ErrorAction SilentlyContinue
        }
        cargo test -p agnoperiph --test visual -- --ignored --nocapture
    }
    "visual-baseline" {
        if (-not [string]::IsNullOrWhiteSpace($Arg)) {
            $env:AGNO_VISUAL = $Arg
        } else {
            Remove-Item Env:AGNO_VISUAL -ErrorAction SilentlyContinue
        }
        $env:AGNO_VISUAL_UPDATE_BASELINE = "1"
        try {
            cargo test -p agnoperiph --test visual -- --ignored --nocapture
        } finally {
            Remove-Item Env:AGNO_VISUAL_UPDATE_BASELINE -ErrorAction SilentlyContinue
        }
    }
    "mock" {
        cargo run -p agnoperiph -- --mock
    }
    "watch-mock" {
        if (-not (Get-Command cargo-watch -ErrorAction SilentlyContinue)) {
            Write-Error "cargo-watch is not installed. Install it with: cargo install cargo-watch"
        }
        cargo watch -w crates/ui/src -w crates/core/src -x "run -p agnoperiph -- --mock"
    }
    "prune" {
        # Only the incremental cache; target\debug and target\release stay intact.
        $incremental = Join-Path $PSScriptRoot "..\target\debug\incremental"
        if (-not (Test-Path $incremental)) {
            Write-Host "Nothing to prune: target\debug\incremental does not exist."
            return
        }
        $bytes = (Get-ChildItem $incremental -Recurse -File -Force -ErrorAction SilentlyContinue |
            Measure-Object -Property Length -Sum).Sum
        Remove-Item $incremental -Recurse -Force
        Write-Host ("Freed {0:N1} MB from target\debug\incremental." -f (($bytes ?? 0) / 1MB))
    }
    "clean-dev" {
        # Preserves target/release/
        cargo clean --profile dev
    }
    "sweep" {
        if (-not (Get-Command cargo-sweep -ErrorAction SilentlyContinue)) {
            Write-Error "cargo-sweep is not installed. Install it with: cargo install cargo-sweep"
        }
        $days = if ([string]::IsNullOrWhiteSpace($Arg)) { "7" } else { $Arg }
        cargo sweep --time $days
    }
    default {
        Write-Host "AgnoPeriph dev tasks:"
        Write-Host "  .\scripts\dev.ps1 check                    Full fmt + clippy + workspace test gate"
        Write-Host "  .\scripts\dev.ps1 test [crate]             Run targeted crate tests (or workspace if omitted)"
        Write-Host "  .\scripts\dev.ps1 visual [filter]          Render headless UI screenshots (optional AGNO_VISUAL filter)"
        Write-Host "  .\scripts\dev.ps1 visual-baseline [filter] Update target/ui-screenshots-baseline/ for pixel diffing"
        Write-Host "  .\scripts\dev.ps1 mock                     Launch AgnoPeriph window against MockPeripheralService"
        Write-Host "  .\scripts\dev.ps1 watch-mock               Auto-rebuild and relaunch --mock on save (needs cargo-watch)"
        Write-Host "  .\scripts\dev.ps1 prune                    Delete only target/debug/incremental and report space freed"
        Write-Host "  .\scripts\dev.ps1 clean-dev                Clean dev profile only (preserves target/release/)"
        Write-Host "  .\scripts\dev.ps1 sweep [days]             Prune stale artifacts older than N days (default: 7; weekly: sweep 14)"
    }
}
