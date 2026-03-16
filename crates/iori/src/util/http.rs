pub use reqwest;
use reqwest::{Client, ClientBuilder, IntoUrl};
use reqwest_cookie_store::{CookieStore, CookieStoreMutex};
use std::sync::{Arc, OnceLock};

pub struct IoriHttp {
    client: OnceLock<Client>,
    builder: Arc<dyn Fn() -> ClientBuilder + Send + Sync + 'static>,
    cookies_store: Arc<CookieStoreMutex>,
}

impl Clone for IoriHttp {
    fn clone(&self) -> Self {
        Self {
            client: OnceLock::new(),
            builder: Arc::clone(&self.builder),
            cookies_store: Arc::clone(&self.cookies_store),
        }
    }
}

impl IoriHttp {
    pub fn new(builder: impl Fn() -> ClientBuilder + Send + Sync + 'static) -> Self {
        let cookies_store = Arc::new(CookieStoreMutex::new(CookieStore::default()));
        Self {
            client: OnceLock::new(),
            builder: Arc::new(builder),
            cookies_store,
        }
    }

    pub fn add_cookies(&self, cookies: Vec<String>, url: impl IntoUrl) {
        if cookies.is_empty() {
            return;
        }

        let url: url::Url = url.into_url().unwrap();
        let mut lock = self.cookies_store.lock().unwrap();
        for cookie in cookies {
            _ = lock.parse(&cookie, &url);
        }
    }

    /// Export all cookies in the store as `name=value` strings for a given URL.
    ///
    /// This returns cookies that would be sent in a request to the given URL,
    /// respecting domain and path matching rules.
    pub fn export_cookies_for_url(&self, url: impl IntoUrl) -> Vec<String> {
        let url: url::Url = url.into_url().unwrap();
        let lock = self.cookies_store.lock().unwrap();
        lock.get_request_values(&url)
            .map(|(name, value)| format!("{name}={value}"))
            .collect()
    }

    pub fn builder(&self) -> ClientBuilder {
        let cookies_store = self.cookies_store.clone();
        (self.builder)().cookie_provider(cookies_store)
    }

    pub fn client(&self) -> Client {
        self.client
            .get_or_init(|| {
                let builder = self.builder();
                builder.build().unwrap()
            })
            .clone()
    }
}
