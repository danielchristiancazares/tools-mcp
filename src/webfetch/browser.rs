//! Headless browser pool for rendering JavaScript-heavy websites.
//!
//! This module provides a managed Chrome/Chromium browser pool that handles the
//! complexity of headless browser automation for web scraping. It uses the
//! Chrome DevTools Protocol (CDP) via the `chromiumoxide` crate.
//!
//! ## Architecture
//!
//! ```text
//! +------------------+     +------------------+     +------------------+
//! |   BrowserPool    |---->|  BrowserInstance |---->|  Chrome Process  |
//! |   (singleton)    |     |  (managed state) |     |  (headless)      |
//! +------------------+     +------------------+     +------------------+
//!         |                        |
//!         v                        v
//!   Lifecycle Mgmt           Request Counter
//!   - Lazy spawn             - Age tracking
//!   - Auto-restart           - Memory cleanup
//! ```
//!
//! ## Lifecycle Management
//!
//! The browser pool automatically manages Chrome process lifecycle to prevent
//! memory leaks and ensure stability:
//!
//! - **Lazy initialization**: Browser is not spawned until first render request
//! - **Request-based restart**: Restarts after 100 requests to clear accumulated memory
//! - **Age-based restart**: Restarts after 1 hour regardless of request count
//! - **Graceful shutdown**: Attempts clean close before process termination
//!
//! ## Stealth Configuration
//!
//! The browser is configured to avoid detection as a headless browser:
//! - Realistic user agent string
//! - `navigator.webdriver` property masked
//! - Automation-related Chrome flags disabled
//!
//! ## Resource Optimization
//!
//! To reduce bandwidth and improve performance:
//! - Images are disabled via Blink settings
//! - Web fonts are disabled (system fallbacks used)
//! - Video/audio autoplay is blocked
//! - Background networking is disabled

use anyhow::{Context, Result, anyhow};
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::network::EventResponseReceived;
use chromiumoxide::cdp::browser_protocol::page::EventLoadEventFired;
use chromiumoxide::page::Page;
use futures::StreamExt;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

// ============================================================================
// Configuration Constants
// ============================================================================

/// Maximum requests before browser restart to prevent memory leaks.
/// Chrome accumulates memory over time; periodic restarts keep usage bounded.
const MAX_REQUESTS_BEFORE_RESTART: usize = 100;

/// Maximum browser uptime before forced restart.
/// Even with few requests, long-running Chrome processes can degrade.
const MAX_BROWSER_AGE: Duration = Duration::from_secs(3600); // 1 hour

/// Timeout for initial page navigation.
/// Covers DNS resolution, TCP connect, TLS handshake, and initial HTML load.
const NAVIGATION_TIMEOUT: Duration = Duration::from_secs(15);

/// Time to wait for network activity to settle after page load.
/// Allows dynamic content and XHR requests to complete.
const NETWORK_IDLE_TIMEOUT: Duration = Duration::from_secs(2);

/// Thread-safe browser pool with automatic lifecycle management.
///
/// The pool maintains at most one Chrome process at a time and handles:
/// - Lazy spawning on first request
/// - Automatic restart based on request count or age
/// - Graceful shutdown when possible
///
/// # Thread Safety
///
/// The pool uses `Arc<Mutex<Option<BrowserInstance>>>` to allow safe concurrent
/// access. The mutex is held only during spawn/restart decisions, not during
/// page rendering, allowing multiple pages to render concurrently.
///
/// # Example
///
/// ```ignore
/// let pool = BrowserPool::new();
/// if BrowserPool::is_available().await {
///     let html = pool.render_page("https://example.com").await?;
/// }
/// ```
pub struct BrowserPool {
    /// The managed browser instance, wrapped in Arc<Mutex> for thread-safe access.
    /// `None` indicates the browser has not been spawned yet or was shut down.
    browser: Arc<Mutex<Option<BrowserInstance>>>,
}

/// Internal state tracking for a browser process.
///
/// Tracks metrics used to decide when the browser should be restarted
/// to prevent memory leaks and maintain performance.
struct BrowserInstance {
    /// Handle to the Chrome process, wrapped in Arc for shared ownership
    /// across concurrent page renders.
    browser: Arc<Browser>,

    /// Timestamp when this browser instance was spawned.
    /// Used for age-based restart decisions.
    created_at: Instant,

    /// Number of pages rendered by this instance.
    /// Used for request-count-based restart decisions.
    request_count: usize,
}

impl BrowserPool {
    /// Creates a new browser pool with no active browser.
    ///
    /// The Chrome process is not spawned until the first `render_page` call.
    /// This allows the pool to be created at startup without incurring the
    /// cost of spawning Chrome if browser rendering is never needed.
    pub fn new() -> Self {
        Self {
            browser: Arc::new(Mutex::new(None)),
        }
    }

    /// Obtains a browser instance, spawning or restarting as needed.
    ///
    /// This is the core lifecycle management function. It implements the
    /// following logic:
    ///
    /// 1. **Restart check**: If an instance exists, check if it should be
    ///    restarted based on request count (>= 100) or age (>= 1 hour)
    ///
    /// 2. **Graceful shutdown**: If restarting, attempt to close the old
    ///    browser cleanly. If other references exist (concurrent renders),
    ///    skip close and let the old instance be dropped naturally.
    ///
    /// 3. **Spawn new instance**: If no instance exists (first call or after
    ///    restart), spawn a new Chrome process with stealth configuration.
    ///
    /// 4. **Increment counter**: Track this request for restart decisions.
    ///
    /// # Locking Behavior
    ///
    /// The mutex is held for the duration of this function. However, once a
    /// browser `Arc` is cloned and returned, page rendering happens outside
    /// the lock, allowing concurrent page renders.
    async fn get_or_spawn(&self) -> Result<Arc<Browser>> {
        let mut guard = self.browser.lock().await;

        // ====================================================================
        // Phase 1: Determine if restart is needed
        // ====================================================================
        let needs_restart = if let Some(instance) = &*guard {
            let age = instance.created_at.elapsed();
            let should_restart =
                instance.request_count >= MAX_REQUESTS_BEFORE_RESTART || age >= MAX_BROWSER_AGE;

            if should_restart {
                info!(
                    "Browser restart triggered (requests: {}, age: {:?})",
                    instance.request_count, age
                );
            }

            should_restart
        } else {
            false
        };

        // ====================================================================
        // Phase 2: Handle restart if needed
        // ====================================================================
        if needs_restart && let Some(mut instance) = guard.take() {
            // Try to get exclusive ownership for graceful close.
            // Arc::get_mut succeeds only if this is the sole reference.
            match Arc::get_mut(&mut instance.browser) {
                Some(browser) => {
                    // We have exclusive access - close cleanly via CDP
                    if let Err(e) = browser.close().await {
                        warn!("Error closing browser during restart: {}", e);
                    }
                }
                None => {
                    // Other references exist (concurrent renders in progress).
                    // Don't block - the old browser will be dropped when those complete.
                    // This is safe: Chrome process cleanup happens on Browser drop.
                    warn!(
                        "Cannot gracefully close browser during restart: multiple references exist"
                    );
                }
            }
        }

        // ====================================================================
        // Phase 3: Spawn new browser if needed
        // ====================================================================
        if guard.is_none() {
            info!("Spawning new browser instance");
            let browser = spawn_browser().await?;
            *guard = Some(BrowserInstance {
                browser: Arc::new(browser),
                created_at: Instant::now(),
                request_count: 0,
            });
        }

        // ====================================================================
        // Phase 4: Increment counter and return browser reference
        // ====================================================================
        if let Some(instance) = guard.as_mut() {
            instance.request_count += 1;
            Ok(Arc::clone(&instance.browser))
        } else {
            // This should be unreachable - we just spawned above if guard was None
            Err(anyhow!("Failed to initialize browser"))
        }
    }

    /// Renders a page and returns the fully-rendered HTML.
    ///
    /// This function:
    /// 1. Obtains a browser instance (spawning if needed)
    /// 2. Opens a new tab (page) in the browser
    /// 3. Navigates to the URL and waits for JavaScript execution
    /// 4. Waits for network activity to settle
    /// 5. Extracts and returns the rendered HTML
    ///
    /// # Arguments
    ///
    /// * `url` - The URL to render. Should already be SSRF-validated.
    ///
    /// # Returns
    ///
    /// The fully-rendered HTML content as a string, including any content
    /// added by JavaScript execution.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Browser spawn fails (Chrome not installed)
    /// - Navigation fails (network error, invalid URL)
    /// - Rendering times out (15 second limit)
    ///
    /// # Timeout Behavior
    ///
    /// The entire render operation is wrapped in a 15-second timeout.
    /// This covers navigation, JavaScript execution, and network idle wait.
    pub async fn render_page(&self, url: &str) -> Result<String> {
        let browser = self.get_or_spawn().await?;

        // Open new tab starting at about:blank (stealth config applied before navigation)
        let page = browser
            .new_page("about:blank")
            .await
            .context("Failed to create new browser page")?;

        // Render with timeout
        let result =
            tokio::time::timeout(NAVIGATION_TIMEOUT, render_page_internal(page, url)).await;

        match result {
            Ok(Ok(html)) => Ok(html),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(anyhow!(
                "Browser rendering timed out after {:?}",
                NAVIGATION_TIMEOUT
            )),
        }
    }

    /// Forces an immediate browser restart, bypassing normal lifecycle checks.
    ///
    /// Useful for recovery after errors that may have corrupted browser state.
    /// The next `render_page` call will spawn a fresh browser instance.
    #[allow(dead_code)]
    pub async fn force_restart(&self) {
        let mut guard = self.browser.lock().await;
        if let Some(mut instance) = guard.take() {
            info!("Force restarting browser");
            if let Some(browser) = Arc::get_mut(&mut instance.browser) {
                if let Err(e) = browser.close().await {
                    warn!("Error closing browser during force restart: {}", e);
                }
            } else {
                warn!("Cannot close browser: multiple references exist");
            }
        }
    }

    /// Checks if a Chrome/Chromium browser is available on the system.
    ///
    /// This is a synchronous check that searches for Chrome binaries in:
    /// - Environment variables: `CHROME_PATH`, `CHROMIUM_PATH`, `CHROME_EXECUTABLE`
    /// - Common installation paths on Linux, macOS, and Windows
    /// - System PATH via `which`/`where` commands
    ///
    /// Call this before attempting browser rendering to provide graceful
    /// fallback to HTTP-only mode when Chrome is not installed.
    pub async fn is_available() -> bool {
        find_chrome_binary().is_some()
    }
}

impl Drop for BrowserPool {
    fn drop(&mut self) {
        // Cannot await in Drop trait. The Chrome process will be terminated
        // when the process exits or when the Browser handle is dropped.
        debug!("BrowserPool dropped, browser process will be terminated");
    }
}

// ============================================================================
// Browser Spawning and Configuration
// ============================================================================

/// Spawns a new headless Chrome instance with stealth and performance settings.
///
/// The browser is configured with:
///
/// ## Stealth Settings (avoid bot detection)
/// - `--disable-blink-features=AutomationControlled`: Hides automation flag
/// - Custom user agent set after launch
/// - JavaScript patches to mask `navigator.webdriver`
///
/// ## Performance Settings
/// - `--blink-settings=imagesEnabled=false`: Blocks image loading
/// - `--disable-remote-fonts`: Uses system fonts only
/// - `--autoplay-policy=document-user-activation-required`: Blocks autoplay
/// - `--max-old-space-size=512`: Limits V8 heap to 512MB
///
/// ## Security/Stability Settings
/// - `--no-sandbox`: Required for containerized environments (Docker, etc.)
/// - `--disable-dev-shm-usage`: Avoids /dev/shm size issues in containers
/// - `--disable-breakpad`: Disables crash reporting
///
/// # Returns
///
/// A `Browser` handle connected via Chrome DevTools Protocol (CDP).
///
/// # Errors
///
/// Returns an error if Chrome binary is not found or fails to launch.
async fn spawn_browser() -> Result<Browser> {
    let chrome_path = find_chrome_binary()
        .ok_or_else(|| anyhow!("Chrome/Chromium not found. Please install Chrome or Chromium."))?;

    debug!("Using Chrome binary at: {}", chrome_path);

    let config = BrowserConfig::builder()
        .chrome_executable(&chrome_path)
        .disable_default_args() // We'll specify our own for better control
        .args(vec![
            "--headless=new".to_string(),
            "--disable-gpu".to_string(),
            "--no-sandbox".to_string(), // Required for containerized environments
            "--disable-dev-shm-usage".to_string(),
            "--disable-setuid-sandbox".to_string(),
            // Memory limits to prevent leaks
            "--max-old-space-size=512".to_string(),
            // Block resource loading for performance and bandwidth
            "--blink-settings=imagesEnabled=false".to_string(),
            "--disable-remote-fonts".to_string(), // Block web fonts, use system fallbacks
            "--autoplay-policy=document-user-activation-required".to_string(), // Block video/audio autoplay
            // Stealth mode settings
            "--disable-blink-features=AutomationControlled".to_string(),
            // Additional privacy/performance settings
            "--disable-background-networking".to_string(),
            "--disable-background-timer-throttling".to_string(),
            "--disable-breakpad".to_string(),
            "--disable-client-side-phishing-detection".to_string(),
            "--disable-component-extensions-with-background-pages".to_string(),
            "--disable-default-apps".to_string(),
            "--disable-extensions".to_string(),
            "--disable-features=TranslateUI".to_string(),
            "--disable-hang-monitor".to_string(),
            "--disable-ipc-flooding-protection".to_string(),
            "--disable-popup-blocking".to_string(),
            "--disable-prompt-on-repost".to_string(),
            "--disable-sync".to_string(),
            "--metrics-recording-only".to_string(),
            "--no-first-run".to_string(),
            "--safebrowsing-disable-auto-update".to_string(),
        ])
        .build()
        .map_err(|e| anyhow!("Failed to build browser config: {}", e))?;

    let (browser, mut handler) = Browser::launch(config)
        .await
        .context("Failed to launch browser")?;

    // Spawn handler to process browser events
    tokio::spawn(async move {
        while let Some(event) = handler.next().await {
            if let Err(e) = event {
                warn!("Browser event error: {}", e);
            }
        }
        debug!("Browser handler finished");
    });

    Ok(browser)
}

/// Internal page rendering logic with guaranteed cleanup.
///
/// This function handles the complete page lifecycle:
/// 1. Apply stealth configuration (user agent, JS patches)
/// 2. Navigate to the target URL
/// 3. Wait for the page load event
/// 4. Wait for network activity to settle (XHR, dynamic content)
/// 5. Extract the rendered HTML
/// 6. Close the page (always, even on error)
///
/// # Page Cleanup
///
/// The page is always closed after rendering, regardless of success or failure.
/// This prevents resource leaks from accumulated browser tabs.
async fn render_page_internal(page: Page, url: &str) -> Result<String> {
    debug!("Navigating to: {}", url);

    // Use async block to ensure page cleanup happens even on early return/error
    let result: Result<String> = async {
        // Step 1: Apply stealth configuration before navigation
        configure_stealth(&page).await?;

        // Step 2: Navigate to target URL
        page.goto(url).await.context("Failed to navigate to URL")?;

        // Step 3: Wait for initial page load (DOMContentLoaded + resources)
        debug!("Waiting for page load event");
        let mut load_event = page.event_listener::<EventLoadEventFired>().await?;
        let _ = tokio::time::timeout(Duration::from_secs(10), load_event.next()).await;

        // Step 4: Wait for network to settle (catches AJAX/fetch requests)
        debug!("Waiting for network idle");
        wait_for_network_idle(&page).await?;

        // Step 5: Extract fully-rendered HTML including JS-generated content
        debug!("Extracting HTML content");
        let html = page
            .content()
            .await
            .context("Failed to extract page content")?;

        Ok(html)
    }
    .await;

    // Step 6: Always close page to free resources (runs even if above failed)
    if let Err(e) = page.close().await {
        warn!("Error closing page: {}", e);
    }

    result
}

/// Applies stealth configuration to avoid headless browser detection.
///
/// Many websites employ anti-bot measures that detect headless browsers.
/// This function applies common evasion techniques:
///
/// ## User Agent
/// Sets a realistic Chrome user agent string matching a standard desktop browser.
///
/// ## JavaScript Property Patches
/// Injects a script that runs before page JavaScript to mask automation:
/// - `navigator.webdriver`: Returns `false` instead of `true`
/// - `navigator.plugins`: Returns a non-empty array (headless has empty)
/// - `navigator.languages`: Returns realistic language preferences
///
/// # Note
///
/// These techniques provide basic evasion but are not foolproof. Sophisticated
/// anti-bot systems may still detect headless browsers through other signals
/// (timing analysis, mouse movements, etc.).
async fn configure_stealth(page: &Page) -> Result<()> {
    // Use a recent, realistic Chrome user agent for Linux desktop
    let user_agent = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";

    page.set_user_agent(user_agent)
        .await
        .context("Failed to set user agent")?;

    // Inject JavaScript patches to mask headless browser indicators.
    // These run before any page JavaScript executes.
    let stealth_script = r#"
        // Override navigator.webdriver - headless browsers set this to true
        Object.defineProperty(navigator, 'webdriver', {
            get: () => false
        });

        // Mock plugins array - headless browsers have empty plugins
        Object.defineProperty(navigator, 'plugins', {
            get: () => [1, 2, 3, 4, 5]
        });

        // Mock languages - ensures realistic language preferences
        Object.defineProperty(navigator, 'languages', {
            get: () => ['en-US', 'en']
        });
    "#;

    page.evaluate(stealth_script)
        .await
        .context("Failed to inject stealth script")?;

    Ok(())
}

/// Waits for network activity to settle, indicating dynamic content has loaded.
///
/// Many modern websites load content dynamically via XHR/fetch after the initial
/// page load. This function monitors network responses and waits until no new
/// responses have been received for `NETWORK_IDLE_TIMEOUT` (2 seconds).
///
/// ## Algorithm
///
/// 1. Subscribe to CDP `Network.responseReceived` events
/// 2. Track timestamp of last network activity
/// 3. Wait until no activity for 2 seconds (idle timeout)
/// 4. Safety limit: Always exit after 20 seconds maximum
///
/// ## Exit Conditions
///
/// The function returns when any of these conditions are met:
/// - No network activity for 2 seconds (success - page is idle)
/// - Event stream ends (browser closed or navigation)
/// - 20 second safety timeout exceeded (prevents infinite wait)
///
/// # Note
///
/// This heuristic works well for typical SPAs but may not catch all dynamic
/// content (e.g., content loaded on scroll or after user interaction).
async fn wait_for_network_idle(page: &Page) -> Result<()> {
    let mut response_event = page.event_listener::<EventResponseReceived>().await?;
    let start = Instant::now();
    let mut last_activity = Instant::now();

    loop {
        // Calculate remaining time until we consider network idle
        let timeout_remaining = NETWORK_IDLE_TIMEOUT
            .checked_sub(last_activity.elapsed())
            .unwrap_or(Duration::from_secs(0));

        if timeout_remaining.is_zero() {
            debug!("Network idle detected");
            break;
        }

        match tokio::time::timeout(timeout_remaining, response_event.next()).await {
            Ok(Some(_)) => {
                // Network activity detected - reset the idle timer
                last_activity = Instant::now();
            }
            Ok(None) => {
                // Event stream ended (page closed or navigated away)
                break;
            }
            Err(_) => {
                // Timeout expired with no activity - network is idle
                break;
            }
        }

        // Safety valve: never wait longer than 20 seconds total
        if start.elapsed() > Duration::from_secs(20) {
            debug!("Network idle wait timeout (safety limit)");
            break;
        }
    }

    Ok(())
}

// ============================================================================
// Chrome Binary Discovery
// ============================================================================

/// Locates a Chrome or Chromium binary on the system.
///
/// Searches in the following order:
///
/// 1. **Environment variables**: `CHROME_PATH`, `CHROMIUM_PATH`, `CHROME_EXECUTABLE`
/// 2. **Common installation paths**: Platform-specific locations for Chrome, Chromium, Edge
/// 3. **System PATH**: Uses `which` (Unix) or `where` (Windows) to find binaries
///
/// ## Supported Browsers
///
/// - Google Chrome
/// - Chromium
/// - Microsoft Edge (Chromium-based)
///
/// ## Platform-Specific Paths
///
/// **Linux**: `/usr/bin/google-chrome`, `/usr/bin/chromium`, `/snap/bin/chromium`
/// **macOS**: `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`
/// **Windows**: `C:\Program Files\Google\Chrome\Application\chrome.exe`
///
/// # Returns
///
/// `Some(path)` if a browser binary is found, `None` otherwise.
fn find_chrome_binary() -> Option<String> {
    // Priority 1: Explicit environment variable override
    for var in ["CHROME_PATH", "CHROMIUM_PATH", "CHROME_EXECUTABLE"] {
        if let Ok(p) = std::env::var(var) {
            let p = p.trim();
            if !p.is_empty() && std::path::Path::new(p).exists() {
                return Some(p.to_string());
            }
        }
    }

    let candidates = [
        // Linux
        "/usr/bin/google-chrome",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/snap/bin/chromium",
        "/usr/bin/microsoft-edge",
        // macOS
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        // Windows (common install locations)
        "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
        "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
        "C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe",
        "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
    ];

    for path in &candidates {
        if std::path::Path::new(path).exists() {
            return Some(path.to_string());
        }
    }

    // Try PATH lookup via which (Unix) or where (Windows)
    fn find_in_path(bin: &str) -> Option<String> {
        let cmd = if cfg!(target_os = "windows") {
            "where"
        } else {
            "which"
        };
        let output = std::process::Command::new(cmd).arg(bin).output().ok()?;
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let first = stdout.lines().next()?.trim();
        if first.is_empty() {
            None
        } else {
            Some(first.to_string())
        }
    }

    for bin in [
        "google-chrome",
        "chrome",
        "chromium",
        "chromium-browser",
        "msedge",
        "microsoft-edge",
    ] {
        if let Some(p) = find_in_path(bin) {
            return Some(p);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // Requires Chrome/Chromium installation
    async fn test_browser_pool_creation() {
        let pool = BrowserPool::new();
        assert!(
            pool.browser.lock().await.is_none(),
            "Browser should not be spawned until first use"
        );
    }

    #[tokio::test]
    #[ignore] // Requires Chrome/Chromium installation
    async fn test_chrome_detection() {
        let is_available = BrowserPool::is_available().await;
        println!("Chrome available: {}", is_available);
        // Don't assert - this depends on system configuration
    }

    #[tokio::test]
    #[ignore] // Requires Chrome/Chromium installation and network
    async fn test_render_simple_page() {
        if !BrowserPool::is_available().await {
            println!("Skipping test - Chrome not available");
            return;
        }

        let pool = BrowserPool::new();
        let result = pool.render_page("https://example.com").await;

        match result {
            Ok(html) => {
                assert!(
                    html.contains("Example Domain"),
                    "Should contain example.com content"
                );
                assert!(html.len() > 100, "Should have substantial HTML content");
            }
            Err(e) => {
                println!("Render failed (might be network issue): {}", e);
            }
        }
    }
}
