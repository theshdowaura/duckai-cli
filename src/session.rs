use std::time::Duration;

use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, ACCEPT_LANGUAGE, ORIGIN, REFERER, USER_AGENT,
};
use reqwest::{Client, ClientBuilder, Url};

use crate::error::Result;
use crate::util::{platform_token, sec_ch_ua};

const BASE_URL: &str = "https://duckduckgo.com";

/// Wrapper around the configured HTTP client and session metadata.
#[derive(Debug, Clone)]
pub struct HttpSession {
    client: Client,
    base: Url,
    user_agent: String,
}

/// Minimal data required to build an HTTP session.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub user_agent: String,
    pub timeout: Duration,
}

impl SessionConfig {
    pub fn new(user_agent: String, timeout: Duration) -> Self {
        Self {
            user_agent,
            timeout,
        }
    }
}

impl HttpSession {
    /// Build a new HTTP session based on CLI arguments.
    pub fn new(config: &SessionConfig) -> Result<Self> {
        let timeout = config.timeout;

        let mut default_headers = HeaderMap::new();
        default_headers.insert(USER_AGENT, HeaderValue::from_str(&config.user_agent)?);
        default_headers.insert(
            ACCEPT_LANGUAGE,
            HeaderValue::from_static("zh-CN,zh;q=0.9,en-US;q=0.8,en;q=0.7"),
        );
        default_headers.insert(
            sec_ch_ua_header(),
            HeaderValue::from_str(&sec_ch_ua(&config.user_agent))?,
        );
        default_headers.insert(sec_ch_ua_mobile_header(), HeaderValue::from_static("?0"));
        default_headers.insert(
            sec_ch_ua_platform_header(),
            HeaderValue::from_str(platform_token(&config.user_agent))?,
        );
        default_headers.insert(ORIGIN, HeaderValue::from_static(BASE_URL));
        default_headers.insert(REFERER, HeaderValue::from_static(BASE_URL));

        let client = ClientBuilder::new()
            .cookie_store(true)
            .default_headers(default_headers)
            .timeout(timeout)
            .pool_idle_timeout(Duration::from_secs(30))
            .user_agent(&config.user_agent)
            .build()?;

        Ok(Self {
            client,
            base: Url::parse(BASE_URL)?,
            user_agent: config.user_agent.clone(),
        })
    }

    /// Returns reference to the inner `reqwest::Client`.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Base DuckDuckGo URL.
    pub fn base_url(&self) -> &Url {
        &self.base
    }

    /// Configured user agent.
    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }
}

fn sec_ch_ua_header() -> HeaderName {
    HeaderName::from_static("sec-ch-ua")
}

fn sec_ch_ua_mobile_header() -> HeaderName {
    HeaderName::from_static("sec-ch-ua-mobile")
}

fn sec_ch_ua_platform_header() -> HeaderName {
    HeaderName::from_static("sec-ch-ua-platform")
}
