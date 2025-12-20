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
