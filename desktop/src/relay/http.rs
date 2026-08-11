use std::error::Error;

use cruisemesh_core::{
    CoreRelayHeader, CoreRelayHttpRequest, CoreRelayHttpResult, CoreRelayTransportError,
};
use futures_util::StreamExt;
use reqwest::{Client, Method};

#[derive(Clone)]
pub struct RelayHttpClient {
    client: Client,
}

impl RelayHttpClient {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(std::time::Duration::from_secs(5))
                .timeout(std::time::Duration::from_secs(15))
                .build()?,
        })
    }

    pub async fn execute(
        &self,
        pass_id: String,
        action_id: u64,
        request: CoreRelayHttpRequest,
    ) -> CoreRelayHttpResult {
        let completed = |status, headers, body, error| CoreRelayHttpResult {
            pass_id: pass_id.clone(),
            action_id,
            status,
            headers,
            body,
            error,
            completed_at_ms: now_ms(),
        };

        let method = match Method::from_bytes(request.method.as_bytes()) {
            Ok(method) => method,
            Err(_) => {
                return completed(0, vec![], vec![], Some(CoreRelayTransportError::Other));
            }
        };
        let url = format!("{}{}", request.base_url, request.path);
        let mut outbound = self.client.request(method, url).body(request.body);
        for header in &request.headers {
            outbound = outbound.header(&header.name, &header.value);
        }
        let response = match outbound.send().await {
            Ok(response) => response,
            Err(error) => {
                return completed(0, vec![], vec![], Some(classify_error(&error)));
            }
        };
        let status = response.status().as_u16();
        let headers = request
            .response_headers_wanted
            .iter()
            .filter_map(|wanted| {
                response
                    .headers()
                    .get(wanted)
                    .and_then(|value| value.to_str().ok())
                    .map(|value| CoreRelayHeader {
                        name: wanted.clone(),
                        value: value.to_owned(),
                    })
            })
            .collect();
        let maximum = request.max_response_bytes as usize;
        if response
            .content_length()
            .is_some_and(|length| length > maximum as u64)
        {
            return completed(
                status,
                headers,
                vec![],
                Some(CoreRelayTransportError::BodyTooLarge),
            );
        }

        let mut stream = response.bytes_stream();
        let mut body = Vec::with_capacity(maximum.min(64 * 1024));
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    return completed(status, headers, vec![], Some(classify_error(&error)));
                }
            };
            if body.len().saturating_add(chunk.len()) > maximum {
                return completed(
                    status,
                    headers,
                    vec![],
                    Some(CoreRelayTransportError::BodyTooLarge),
                );
            }
            body.extend_from_slice(&chunk);
        }
        completed(status, headers, body, None)
    }
}

fn classify_error(error: &reqwest::Error) -> CoreRelayTransportError {
    if error.is_timeout() {
        CoreRelayTransportError::Timeout
    } else if error.is_connect() {
        CoreRelayTransportError::ConnectionFailed
    } else if error
        .source()
        .is_some_and(|source| source.to_string().to_ascii_lowercase().contains("tls"))
    {
        CoreRelayTransportError::Tls
    } else {
        CoreRelayTransportError::Other
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn malformed_method_is_returned_to_core_as_a_transport_error() {
        let request = CoreRelayHttpRequest {
            operation: cruisemesh_core::CoreRelayOperation::FetchPage,
            method: "not a method\n".into(),
            base_url: "https://relay.invalid".into(),
            path: "/v1/envelopes".into(),
            headers: vec![],
            body: vec![],
            max_response_bytes: 10,
            response_headers_wanted: vec![],
        };
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let result = runtime.block_on(RelayHttpClient::new().unwrap().execute(
            "p".into(),
            1,
            request,
        ));
        assert_eq!(result.error, Some(CoreRelayTransportError::Other));
    }

    #[tokio::test]
    async fn declared_oversize_body_is_rejected_before_accumulation() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1_024];
            let _ = stream.read(&mut request).await.unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\nhello world")
                .await
                .unwrap();
        });
        let request = CoreRelayHttpRequest {
            operation: cruisemesh_core::CoreRelayOperation::FetchPage,
            method: "GET".into(),
            base_url: format!("http://{address}"),
            path: "/v1/envelopes".into(),
            headers: vec![],
            body: vec![],
            max_response_bytes: 10,
            response_headers_wanted: vec![],
        };
        let result = RelayHttpClient::new()
            .unwrap()
            .execute("p".into(), 1, request)
            .await;
        assert_eq!(result.error, Some(CoreRelayTransportError::BodyTooLarge));
        assert!(result.body.is_empty());
    }
}
