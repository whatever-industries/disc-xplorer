use reqwest::{redirect, Client, Response, Url};
use std::{sync::OnceLock, time::Duration};

pub(crate) const AGENT: &str = concat!(
    "Disc-Xplorer/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/whatever-industries/disc-xplorer)"
);

pub(crate) fn client(cover_art: bool) -> Result<&'static Client, String> {
    static API: OnceLock<Result<Client, String>> = OnceLock::new();
    static COVER: OnceLock<Result<Client, String>> = OnceLock::new();
    let slot = if cover_art { &COVER } else { &API };
    slot.get_or_init(|| {
        let policy = if cover_art {
            redirect::Policy::custom(|attempt| {
                if attempt.previous().len() < 6 && cover_url(attempt.url()) {
                    attempt.follow()
                } else {
                    attempt.error("Untrusted cover artwork redirect")
                }
            })
        } else {
            redirect::Policy::none()
        };
        Client::builder()
            .user_agent(AGENT)
            .redirect(policy)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(12))
            .build()
            .map_err(|e| e.to_string())
    })
    .as_ref()
    .map_err(Clone::clone)
}

fn cover_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.host_str().is_some_and(|host| {
            host == "coverartarchive.org" || host == "archive.org" || host.ends_with(".archive.org")
        })
}

pub(crate) async fn body(mut response: Response, max_size: usize) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|n| n > max_size as u64)
    {
        return Err("The online service returned too much data.".into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if chunk.len() > max_size - body.len() {
            return Err("The online service returned too much data.".into());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
