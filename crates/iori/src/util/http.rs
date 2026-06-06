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

    pub fn clear_cookies_by_names(&self, names: &[&str]) -> usize {
        let mut lock = self.cookies_store.lock().unwrap();
        let to_remove: Vec<(String, String, String)> = lock
            .iter_any()
            .filter(|cookie| names.contains(&cookie.name()))
            .filter_map(|cookie| {
                Some((
                    cookie.domain()?.to_string(),
                    cookie.path()?.to_string(),
                    cookie.name().to_string(),
                ))
            })
            .collect();

        for (domain, path, name) in &to_remove {
            let _ = lock.remove(domain, path, name);
        }

        to_remove.len()
    }

    pub fn clear_all_cookies(&self) {
        let mut lock = self.cookies_store.lock().unwrap();
        *lock = CookieStore::default();
    }

    pub fn snapshot_cookies(&self) -> CookieStore {
        self.cookies_store.lock().unwrap().clone()
    }

    pub fn restore_cookies(&self, snapshot: CookieStore) {
        let mut lock = self.cookies_store.lock().unwrap();
        *lock = snapshot;
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

    pub fn raw_builder(&self) -> ClientBuilder {
        (self.builder)()
    }

    pub fn client(&self) -> Client {
        self.client
            .get_or_init(|| {
                let builder = self.builder();
                builder.build().unwrap()
            })
            .clone()
    }

    pub fn raw_client(&self) -> Client {
        self.raw_builder().build().unwrap()
    }
}
