use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Duration;

use reqwest::Client;

use crate::config::ProxyMode;

static MODE: RwLock<ProxyMode> = RwLock::new(ProxyMode::System);
static CLIENT: RwLock<Option<(ProxyMode, Client)>> = RwLock::new(None);

fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|error| error.into_inner())
}

fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(|error| error.into_inner())
}

pub(crate) fn set_proxy_mode(mode: ProxyMode) {
    *write(&MODE) = mode;
}

pub(crate) fn builder() -> reqwest::ClientBuilder {
    builder_for(*read(&MODE))
}

pub(crate) fn builder_for(mode: ProxyMode) -> reqwest::ClientBuilder {
    let builder = Client::builder();
    match mode {
        ProxyMode::System => crate::shell_env::environment_proxies().apply(builder),
        ProxyMode::Env => {
            let proxies = crate::shell_env::environment_proxies();
            if proxies.is_empty() {
                builder.no_proxy()
            } else {
                proxies.apply(builder)
            }
        }
        ProxyMode::Direct => builder.no_proxy(),
    }
}

fn cached(mode: ProxyMode) -> Option<Client> {
    let slot = read(&CLIENT);
    let (cached, client) = slot.as_ref()?;
    (*cached == mode).then(|| client.clone())
}

pub(crate) fn client() -> Result<Client, reqwest::Error> {
    let mode = *read(&MODE);
    if let Some(client) = cached(mode) {
        return Ok(client);
    }
    let client = builder_for(mode)
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(120))
        .build()?;
    if crate::shell_env::ready() {
        *write(&CLIENT) = Some((mode, client.clone()));
    }
    Ok(client)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use crate::config::ProxyMode;

    #[tokio::test]
    async fn reuses_one_connection_for_repeated_requests() {
        super::set_proxy_mode(ProxyMode::Direct);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let accepted = Arc::new(AtomicUsize::new(0));
        let counter = accepted.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                counter.fetch_add(1, Ordering::SeqCst);
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                std::thread::spawn(move || {
                    loop {
                        let mut length = 0usize;
                        loop {
                            let mut line = String::new();
                            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                                return;
                            }
                            if line == "\r\n" {
                                break;
                            }
                            if let Some(value) =
                                line.to_ascii_lowercase().strip_prefix("content-length: ")
                            {
                                length = value.trim().parse().unwrap_or(0);
                            }
                        }
                        let mut body = vec![0_u8; length];
                        if reader.read_exact(&mut body).is_err() {
                            return;
                        }
                        let payload = b"{\"ok\":true}";
                        let head = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
                            payload.len()
                        );
                        if stream.write_all(head.as_bytes()).is_err()
                            || stream.write_all(payload).is_err()
                        {
                            return;
                        }
                    }
                });
            }
        });

        let client = super::client().unwrap();
        let url = format!("http://{address}/ping");
        for _ in 0..2 {
            let response = client.post(&url).body("{}").send().await.unwrap();
            assert!(response.status().is_success());
            assert_eq!(response.text().await.unwrap(), "{\"ok\":true}");
        }
        assert_eq!(accepted.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn the_cached_client_belongs_to_one_proxy_mode() {
        let client = super::builder_for(ProxyMode::Direct).build().unwrap();
        *super::write(&super::CLIENT) = Some((ProxyMode::Direct, client));
        assert!(super::cached(ProxyMode::Direct).is_some());
        assert!(super::cached(ProxyMode::Env).is_none());
    }
}
