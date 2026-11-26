use std::sync::OnceLock;
use std::time::Duration;

use reqwest::Client;

static CLIENT: OnceLock<Client> = OnceLock::new();

pub(crate) fn client() -> Result<Client, reqwest::Error> {
    if let Some(client) = CLIENT.get() {
        return Ok(client.clone());
    }
    let client = crate::shell_env::configure_http(Client::builder())
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(120))
        .build()?;
    if crate::shell_env::ready() {
        let _ = CLIENT.set(client.clone());
    }
    Ok(client)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn reuses_one_connection_for_repeated_requests() {
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
}
