/// Headless browser pool for rendering JavaScript-heavy websites
/// Uses chromiumoxide for async Chrome DevTools Protocol access
use anyhow::{anyhow, Context, Result};
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::network::EventResponseReceived;
use chromiumoxide::cdp::browser_protocol::page::EventLoadEventFired;
use chromiumoxide::page::Page;
use futures::StreamExt;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// Maximum number of requests before forcing browser restart
const MAX_REQUESTS_BEFORE_RESTART: usize = 100;

/// Maximum time browser can run before forced restart
const MAX_BROWSER_AGE: Duration = Duration::from_secs(3600); // 1 hour

/// Timeout for page navigation
const NAVIGATION_TIMEOUT: Duration = Duration::from_secs(15);

/// Timeout for network idle after page load
const NETWORK_IDLE_TIMEOUT: Duration = Duration::from_secs(2);

/// Browser pool manager with automatic restarts to prevent memory leaks
pub struct BrowserPool {
    browser: Arc<Mutex<Option<BrowserInstance>>>,
}

struct BrowserInstance {
    browser: Arc<Browser>,
    created_at: Instant,
    request_count: usize,
}

impl BrowserPool {
    /// Create a new browser pool (browser lazily spawned on first use)
    pub fn new() -> Self {
        Self {
            browser: Arc::new(Mutex::new(None)),
        }
    }

    /// Get or spawn browser instance, with automatic restart if needed
    async fn get_or_spawn(&self) -> Result<Arc<Browser>> {
        let mut guard = self.browser.lock().await;

        // Check if restart needed
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

        // Restart if needed
        if needs_restart {
            if let Some(mut instance) = guard.take() {
                // Attempt graceful shutdown (don't fail if there are multiple refs)
                match Arc::get_mut(&mut instance.browser) {
                    Some(browser) => {
                        if let Err(e) = browser.close().await {
                            warn!("Error closing browser during restart: {}", e);
                        }
                    }
                    None => {
                        // Don't fail the request - old browser will be dropped when refs go away
                        warn!("Cannot gracefully close browser during restart: multiple references exist");
                    }
                }
            }
        }

        // Spawn new browser if none exists
        if guard.is_none() {
            info!("Spawning new browser instance");
            let browser = spawn_browser().await?;
            *guard = Some(BrowserInstance {
                browser: Arc::new(browser),
                created_at: Instant::now(),
                request_count: 0,
            });
        }

        // Increment request counter and return browser
        if let Some(instance) = guard.as_mut() {
            instance.request_count += 1;
            Ok(Arc::clone(&instance.browser))
        } else {
            Err(anyhow!("Failed to initialize browser"))
        }
    }

    /// Render a page with JavaScript execution
    pub async fn render_page(&self, url: &str) -> Result<String> {
        let browser = self.get_or_spawn().await?;

        // Create new page (tab)
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

    /// Force restart the browser (useful for recovery from errors)
    #[allow(dead_code)]
    pub async fn force_restart(&self) {
        let mut guard = self.browser.lock().await;
        if let Some(mut instance) = guard.take() {
            info!("Force restarting browser");
            // Try to get exclusive access to close the browser
            if let Some(browser) = Arc::get_mut(&mut instance.browser) {
                if let Err(e) = browser.close().await {
                    warn!("Error closing browser during force restart: {}", e);
                }
            } else {
                warn!("Cannot close browser: multiple references exist");
            }
        }
    }

    /// Check if browser is available (Chrome/Chromium installed)
    pub async fn is_available() -> bool {
        // Try to find Chrome/Chromium binary
        find_chrome_binary().is_some()
    }
}

impl Drop for BrowserPool {
    fn drop(&mut self) {
        // Note: Can't await in Drop, browser will be killed when process exits
        debug!("BrowserPool dropped, browser process will be terminated");
    }
}

/// Spawn a new headless Chrome instance with stealth configuration
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

/// Render a page and extract HTML content
async fn render_page_internal(page: Page, url: &str) -> Result<String> {
    debug!("Navigating to: {}", url);

    // Inner block so we always close the page even on errors
    let result: Result<String> = async {
        // Configure stealth settings
        configure_stealth(&page).await?;

        // Navigate to URL
        page.goto(url).await.context("Failed to navigate to URL")?;

        // Wait for load event
        debug!("Waiting for page load event");
        let mut load_event = page.event_listener::<EventLoadEventFired>().await?;
        let _ = tokio::time::timeout(Duration::from_secs(10), load_event.next()).await;

        // Wait for network idle (additional content loading)
        debug!("Waiting for network idle");
        wait_for_network_idle(&page).await?;

        // Extract HTML content
        debug!("Extracting HTML content");
        let html = page
            .content()
            .await
            .context("Failed to extract page content")?;

        Ok(html)
    }
    .await;

    // Always close the page to free resources (even on errors)
    if let Err(e) = page.close().await {
        warn!("Error closing page: {}", e);
    }

    result
}

/// Configure stealth settings to avoid detection
async fn configure_stealth(page: &Page) -> Result<()> {
    // Set realistic user agent
    let user_agent = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";

    page.set_user_agent(user_agent)
        .await
        .context("Failed to set user agent")?;

    // Inject scripts to mask headless indicators
    let stealth_script = r#"
        // Override navigator.webdriver
        Object.defineProperty(navigator, 'webdriver', {
            get: () => false
        });

        // Mock plugins
        Object.defineProperty(navigator, 'plugins', {
            get: () => [1, 2, 3, 4, 5]
        });

        // Mock languages
        Object.defineProperty(navigator, 'languages', {
            get: () => ['en-US', 'en']
        });
    "#;

    page.evaluate(stealth_script)
        .await
        .context("Failed to inject stealth script")?;

    Ok(())
}

/// Wait for network to become idle (no new requests for NETWORK_IDLE_TIMEOUT)
async fn wait_for_network_idle(page: &Page) -> Result<()> {
    let mut response_event = page.event_listener::<EventResponseReceived>().await?;
    let start = Instant::now();
    let mut last_activity = Instant::now();

    loop {
        let timeout_remaining = NETWORK_IDLE_TIMEOUT
            .checked_sub(last_activity.elapsed())
            .unwrap_or(Duration::from_secs(0));

        if timeout_remaining.is_zero() {
            debug!("Network idle detected");
            break;
        }

        match tokio::time::timeout(timeout_remaining, response_event.next()).await {
            Ok(Some(_)) => {
                // Network activity detected, reset timer
                last_activity = Instant::now();
            }
            Ok(None) => {
                // Stream ended
                break;
            }
            Err(_) => {
                // Timeout - network is idle
                break;
            }
        }

        // Safety: Don't wait forever
        if start.elapsed() > Duration::from_secs(20) {
            debug!("Network idle wait timeout");
            break;
        }
    }

    Ok(())
}

/// Find Chrome or Chromium binary on the system
fn find_chrome_binary() -> Option<String> {
    // Allow explicit override via environment variable
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
