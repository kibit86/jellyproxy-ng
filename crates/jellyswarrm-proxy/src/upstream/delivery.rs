//! Redirect or stream media from a selected backend.

use std::{io, pin::Pin};

use futures_util::{Stream, StreamExt};
use hyper::{body::Bytes, header, HeaderMap, StatusCode};

use super::hls;
use crate::upstream::headers::remove_hop_by_hop_headers;

#[derive(thiserror::Error)]
pub(crate) enum DeliveryError {
    #[error("upstream transport failure")]
    Transport(#[source] reqwest::Error),
    #[error("playlist delivery failed")]
    Playlist(#[from] hls::HlsError),
}

impl std::fmt::Debug for DeliveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

/// Concrete transport dependencies, with authorization resolved by the caller.
pub(crate) struct DeliveryContext<'a> {
    streaming_client: &'a reqwest::Client,
    hls: hls::HlsContext<'a>,
}

impl<'a> DeliveryContext<'a> {
    pub(crate) fn new(streaming_client: &'a reqwest::Client, hls: hls::HlsContext<'a>) -> Self {
        Self {
            streaming_client,
            hls,
        }
    }
}

pub(crate) type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, io::Error>> + Send>>;

pub(crate) enum DeliveryBody {
    Empty,
    Stream(ByteStream),
    Playlist(String),
}

pub(crate) struct DeliveryResponse {
    pub(crate) status: StatusCode,
    pub(crate) headers: HeaderMap,
    pub(crate) body: DeliveryBody,
}

/// Execute a resolved request without selecting or changing its backend.
/// Binary bodies are never collected; only HLS playlists are buffered.
pub(crate) async fn proxy(
    context: &DeliveryContext<'_>,
    mut request: reqwest::Request,
) -> Result<DeliveryResponse, DeliveryError> {
    let playlist_path = hls::is_playlist_path(request.url());
    let head = request.method() == reqwest::Method::HEAD;
    request.headers_mut().insert(
        header::ACCEPT_ENCODING,
        header::HeaderValue::from_static("identity"),
    );
    if playlist_path {
        // Never serve a partial playlist or revalidate a pre-rewrite version.
        for header in [
            header::RANGE,
            header::IF_RANGE,
            header::IF_NONE_MATCH,
            header::IF_MODIFIED_SINCE,
        ] {
            request.headers_mut().remove(header);
        }
    }
    let response = context
        .streaming_client
        .execute(request)
        .await
        .map_err(|error| DeliveryError::Transport(error.without_url()))?;

    if playlist_path || hls::is_playlist_type(response.headers()) {
        let playlist = hls::response(&context.hls, response, head).await?;
        return Ok(DeliveryResponse {
            status: StatusCode::OK,
            headers: playlist.headers,
            body: if head {
                DeliveryBody::Empty
            } else {
                DeliveryBody::Playlist(playlist.body)
            },
        });
    }

    let status = response.status();
    let mut headers = response.headers().clone();
    remove_hop_by_hop_headers(&mut headers);
    let stream = response
        .bytes_stream()
        .map(|result| result.map_err(|error| io::Error::other(error.without_url())));
    Ok(DeliveryResponse {
        status,
        headers,
        body: DeliveryBody::Stream(Box::pin(stream)),
    })
}
