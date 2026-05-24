set windows-shell := ["pwsh", "-NoProfile", "-Command"]

# Install tools-mcp-server to ~/.cargo/bin
install:
    cargo install --path tools-mcp-server

# Build in release mode
build:
    cargo build --release

# Build in debug mode
build-debug:
    cargo build

# Run tests
test:
    cargo test

# Run clippy lints
lint:
    cargo clippy --workspace -- -D warnings

# Format code
fmt:
    cargo fmt --all

# Check formatting without modifying
fmt-check:
    cargo fmt --all -- --check

# Run the server locally (debug build)
run:
    cargo run -p tools-mcp-server

# Clean build artifacts
clean:
    cargo clean

# Zip the working tree into <output> (default: tools-mcp.zip), tailored for
# feeding into static-analysis / deep-reasoning tooling.
#
# Captures the CURRENT working-tree state (not HEAD), so uncommitted edits and
# brand-new untracked files matching the pathspecs are included. File
# enumeration uses `git ls-files --cached --others --exclude-standard` so
# .gitignore is respected and ignored noise (target/, .tools-mcp/, IDE caches)
# stays out.
#
# Includes per workspace member: crate manifest + every *.rs (src/, build.rs,
# tests/, benches/). Plus workspace Cargo.toml, Cargo.lock (gitignored —
# pulled in explicitly), project docs (AGENTS.md, README.md, DESIGN.md,
# CHANGELOG.md, docs/**), scripts/**, justfile, .gitattributes, and
# .cargo/config.toml.
#
# Excluded: target/, .agents/, IDE config (.cursor/, .vscode/, .claude/,
# .idea/, .codex, .cursorrules), .tools-mcp/, report.md.
[script("pwsh", "-NoProfile", "-File")]
zip output="tools-mcp.zip":
    $ErrorActionPreference = 'Stop'
    $output = '{{output}}'

    if (-not (Test-Path Cargo.lock)) { cargo generate-lockfile }

    # Enumerate tracked + untracked-not-ignored files matching the pathspecs.
    # Untracked-but-not-ignored is included so a freshly-added crate or doc
    # the user hasn't committed yet still lands in the archive.
    $listed = & git ls-files --cached --others --exclude-standard -- `
        Cargo.toml justfile AGENTS.md README.md DESIGN.md CHANGELOG.md .gitattributes `
        ':(glob).cargo/config.toml' ':(glob)docs/**' ':(glob)scripts/**' `
        ':(glob)tools-mcp-*/Cargo.toml' ':(glob)tools-mcp-*/**/*.rs'
    if ($LASTEXITCODE -ne 0) { throw 'git ls-files failed' }

    # Cargo.lock is gitignored, so add it explicitly from the working tree.
    $files = @($listed) + @('Cargo.lock') |
        Where-Object { $_ -and (Test-Path -LiteralPath $_) } |
        Sort-Object -Unique

    # Stage into a temp tools-mcp/ tree so Compress-Archive yields the right
    # archive-root prefix and entry layout.
    $stage = Join-Path ([System.IO.Path]::GetTempPath()) ('tools-mcp-stage-' + [Guid]::NewGuid().ToString('N'))
    $root = Join-Path $stage 'tools-mcp'
    New-Item -ItemType Directory -Path $root -Force | Out-Null
    try {
        foreach ($file in $files) {
            $dest = Join-Path $root $file
            $destDir = Split-Path -Parent $dest
            if (-not (Test-Path -LiteralPath $destDir)) {
                New-Item -ItemType Directory -Path $destDir -Force | Out-Null
            }
            Copy-Item -LiteralPath $file -Destination $dest -Force
        }
        if (Test-Path -LiteralPath $output) { Remove-Item -LiteralPath $output -Force }
        Compress-Archive -Path $root -DestinationPath $output -CompressionLevel Optimal
    } finally {
        Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue
    }

    $size = (Get-Item -LiteralPath $output).Length
    Write-Host ('wrote {0} ({1:N1} KB, {2} files)' -f $output, ($size / 1KB), $files.Count)
