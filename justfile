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

# Zip workspace source at HEAD into <output> (default: tools-mcp.zip).
# Includes only the workspace Cargo.toml, each crate's Cargo.toml, and every
# tracked *.rs under tools-mcp-*. Everything else (docs, scripts, IDE config,
# .agents/, build artifacts) is excluded. Uncommitted changes are not included.
zip output="tools-mcp.zip":
    git archive --format=zip --prefix=tools-mcp/ --output={{quote(output)}} HEAD -- Cargo.toml ':(glob)tools-mcp-*/Cargo.toml' ':(glob)tools-mcp-*/**/*.rs'
