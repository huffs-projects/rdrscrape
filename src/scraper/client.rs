//! Blocking HTTP client with configurable politeness (delay between requests) and optional retries.

use std::time::{Duration, Instant};

const DEFAULT_USER_AGENT: &str =
    "Mozilla/5.0 (compatible; rdrscrape/0.1; +https://github.com/rdrscrape)";
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const DEFAULT_DELAY_SECS: u64 = 4;
const MAX_REDIRECTS: usize = 10;

/// Default number of attempts for get_with_retry (initial plus retries).
const DEFAULT_RETRY_COUNT: u32 = 5;
/// Default backoff delays in seconds after each failed attempt (1s, 2s, 4s, 8s).
const DEFAULT_BACKOFF_SECS: [u64; 4] = [1, 2, 4, 8];
/// Backoff for HTTP 429 (rate limit): wait longer so the server can recover.
const BACKOFF_429_SECS: [u64; 4] = [30, 60, 90, 120];

/// Blocking HTTP client that enforces a delay between requests.
#[derive(Debug)]
pub struct PoliteClient {
    inner: reqwest::blocking::Client,
    delay: Duration,
    last_request: Option<Instant>,
    retry_count: u32,
    backoff_secs: Vec<u64>,
}

impl PoliteClient {
    /// Build a polite client with default User-Agent, timeout, and delay.
    pub fn new() -> Result<Self, reqwest::Error> {
        Self::builder().build()
    }

    /// Builder for custom User-Agent and/or delay.
    pub fn builder() -> PoliteClientBuilder {
        PoliteClientBuilder::default()
    }

    /// Perform a GET request. Sleeps until the configured delay has passed since the last request.
    pub fn get(&mut self, url: &str) -> Result<reqwest::blocking::Response, reqwest::Error> {
        self.wait_delay();
        let response = self.inner.get(url).send()?;
        self.last_request = Some(Instant::now());
        Ok(response)
    }

    /// Perform a POST request with form data. Sleeps until the configured delay has passed.
    pub fn post_form(
        &mut self,
        url: &str,
        form: &[(&str, &str)],
    ) -> Result<reqwest::blocking::Response, reqwest::Error> {
        self.wait_delay();
        let response = self.inner.post(url).form(form).send()?;
        self.last_request = Some(Instant::now());
        Ok(response)
    }

    /// Perform a GET request with retries for transient failures.
    ///
    /// Retries on: timeout, connection errors, HTTP 5xx, and HTTP 429. Attempt count
    /// and backoff delays are configurable via the builder. Non-retryable errors
    /// (e.g. 4xx except 429) are returned immediately. On success or after exhausting
    /// retries, updates the last-request time for politeness.
    pub fn get_with_retry(
        &mut self,
        url: &str,
    ) -> Result<reqwest::blocking::Response, reqwest::Error> {
        let max_attempts = self.retry_count;
        let mut last_err: Option<reqwest::Error> = None;
        for attempt in 0..max_attempts {
            self.wait_delay();
            match self.inner.get(url).send() {
                Ok(response) => {
                    let status = response.status();
                    let retryable_status = status.is_server_error() || status.as_u16() == 429;
                    if retryable_status && attempt < max_attempts - 1 {
                        last_err = Some(response.error_for_status().unwrap_err());
                        let backoff = if status.as_u16() == 429 {
                            BACKOFF_429_SECS
                                .get(attempt as usize)
                                .copied()
                                .unwrap_or(*BACKOFF_429_SECS.last().unwrap_or(&60))
                        } else {
                            self.backoff_secs
                                .get(attempt as usize)
                                .copied()
                                .unwrap_or_else(|| *self.backoff_secs.last().unwrap_or(&1))
                        };
                        std::thread::sleep(Duration::from_secs(backoff));
                        continue;
                    }
                    self.last_request = Some(Instant::now());
                    return Ok(response);
                }
                Err(e) => {
                    let retryable = e.is_timeout() || e.is_connect();
                    if retryable && attempt < max_attempts - 1 {
                        last_err = Some(e);
                        let backoff = self
                            .backoff_secs
                            .get(attempt as usize)
                            .copied()
                            .unwrap_or_else(|| *self.backoff_secs.last().unwrap_or(&1));
                        std::thread::sleep(Duration::from_secs(backoff));
                        continue;
                    }
                    return Err(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| self.inner.get("http://[::1]:0/").send().unwrap_err()))
    }

    fn wait_delay(&mut self) {
        if let Some(last) = self.last_request {
            let elapsed = last.elapsed();
            if elapsed < self.delay {
                std::thread::sleep(self.delay - elapsed);
            }
        }
    }
}

/// Builder for PoliteClient with optional User-Agent, delay, timeout, and retry settings.
#[derive(Debug)]
pub struct PoliteClientBuilder {
    user_agent: Option<String>,
    delay_secs: u64,
    timeout_secs: u64,
    retry_count: u32,
    retry_backoff_secs: Vec<u64>,
}

impl Default for PoliteClientBuilder {
    fn default() -> Self {
        Self {
            user_agent: None,
            delay_secs: DEFAULT_DELAY_SECS,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            retry_count: DEFAULT_RETRY_COUNT,
            retry_backoff_secs: DEFAULT_BACKOFF_SECS.to_vec(),
        }
    }
}

impl PoliteClientBuilder {
    /// Set a custom User-Agent. If not set, a browser-like default is used.
    pub fn user_agent(mut self, ua: impl Into<String>) -> Self {
        self.user_agent = Some(ua.into());
        self
    }

    /// Set delay between requests in seconds. Default 2.
    pub fn delay_secs(mut self, secs: u64) -> Self {
        self.delay_secs = secs;
        self
    }

    /// Set request timeout in seconds. Default 30.
    pub fn timeout_secs(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    /// Set number of HTTP attempts for transient failures (default 3).
    pub fn retry_count(mut self, n: u32) -> Self {
        self.retry_count = n.max(1);
        self
    }

    /// Set backoff delays in seconds before each retry (e.g. [1, 2, 4]). Length should be retry_count - 1; if shorter, last value is reused.
    pub fn retry_backoff_secs(mut self, secs: Vec<u64>) -> Self {
        self.retry_backoff_secs = secs;
        self
    }

    /// Build the blocking client and polite wrapper.
    pub fn build(self) -> Result<PoliteClient, reqwest::Error> {
        let user_agent = self
            .user_agent
            .unwrap_or_else(|| DEFAULT_USER_AGENT.to_string());
        let inner = reqwest::blocking::Client::builder()
            .cookie_store(true)
            .user_agent(user_agent)
            .timeout(Duration::from_secs(self.timeout_secs))
            .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
            .build()?;
        let backoff_secs = if self.retry_backoff_secs.is_empty() {
            // Default exponential: 1, 2, 4, ... for (retry_count - 1) steps
            let n = self.retry_count.saturating_sub(1) as usize;
            (0..n).map(|i| 1u64 << i.min(4)).collect::<Vec<_>>()
        } else {
            self.retry_backoff_secs
        };
        Ok(PoliteClient {
            inner,
            delay: Duration::from_secs(self.delay_secs),
            last_request: None,
            retry_count: self.retry_count,
            backoff_secs,
        })
    }
}
