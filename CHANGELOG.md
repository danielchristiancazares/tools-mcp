# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Headless browser rendering for JavaScript-heavy websites via chromiumoxide
- 5-heuristic detection system for client-side rendered applications:
  - Empty SPA shell detection (React, Vue, Angular root divs)
  - Script tag density analysis
  - Framework signature detection (data-reactroot, __NEXT_DATA__, etc.)
  - Explicit noscript warnings
  - Content-to-HTML ratio analysis
- Domain whitelist for known JS-heavy sites (Medium, Notion, React docs, etc.)
- Hybrid rendering strategy: HTTP-first with automatic browser fallback
- Browser pool with automatic restart (every 100 requests or 1 hour)
- `force_browser` parameter for explicit browser rendering control
- Separate cache keys for HTTP vs browser-rendered content (`{url}_http` / `{url}_browser`)
- `rendering_method` field in FetchResponse ("http" or "browser")
- Graceful degradation when Chrome/Chromium not installed
- Security documentation for prompt injection mitigation from web content
- SSRF validation applied to both HTTP and browser rendering paths
- Stealth browser configuration to avoid headless detection
- Network idle waiting for dynamic content loading

### Changed
- WebFetch now supports two rendering modes instead of HTTP-only
- Cache keys now include rendering method to prevent conflicts
- FetchResponse includes rendering method metadata for transparency

### Fixed
- Improved SSRF protection with DNS resolution for hostname validation

## [0.9.0] - Previous Release
Initial implementation with MCP server, OpenAI vector store integration, and HTTP-based WebFetch.
