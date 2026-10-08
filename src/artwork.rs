//! Album art, fetched in the background and drawn with whatever graphics the terminal supports.

use std::{collections::VecDeque, time::Duration};

use anyhow::{Context, Result};
use image::DynamicImage;
use ratatui_image::{FontSize, picker::Picker, protocol::StatefulProtocol};
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::{debug, warn};
use url::Url;

const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// How many images to keep, so that switching between groups doesn't fetch them again.
const CACHE_SIZE: usize = 8;

/// A fetched image, or why it couldn't be fetched.
pub type Loaded = (Url, Result<DynamicImage>);

pub struct Artwork {
    picker: Picker,
    client: reqwest::Client,
    loaded_tx: mpsc::UnboundedSender<Loaded>,
    /// The image to show, if any.
    wanted: Option<Url>,
    /// The image to draw, once `wanted` is loaded.
    image: Option<StatefulProtocol>,
    fetch: Option<JoinHandle<()>>,
    /// Recently loaded images, most recent last.
    cache: VecDeque<(Url, DynamicImage)>,
}

impl Artwork {
    /// Draw images with `picker`. Fetched images are sent to the returned receiver, to be passed to
    /// [`Artwork::loaded`].
    pub fn new(picker: Picker) -> (Self, mpsc::UnboundedReceiver<Loaded>) {
        let (loaded_tx, loaded_rx) = mpsc::unbounded_channel();
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .build()
            .expect("the HTTP client's TLS backend can be initialized");
        let artwork = Self {
            picker,
            client,
            loaded_tx,
            wanted: None,
            image: None,
            fetch: None,
            cache: VecDeque::new(),
        };
        (artwork, loaded_rx)
    }

    /// Show the image at `url`, fetching it unless it was loaded recently.
    pub fn show(&mut self, url: Option<&Url>) {
        if self.wanted.as_ref() == url {
            return;
        }
        self.wanted = url.cloned();
        self.image = None;
        if let Some(fetch) = self.fetch.take() {
            fetch.abort();
        }
        let Some(url) = url else { return };

        if let Some((_, image)) = self.cache.iter().find(|(cached, _)| cached == url) {
            self.image = Some(self.picker.new_resize_protocol(image.clone()));
            return;
        }
        debug!("Fetching {url}");
        let client = self.client.clone();
        let loaded_tx = self.loaded_tx.clone();
        let url = url.clone();
        self.fetch = Some(tokio::spawn(async move {
            let image = fetch(&client, &url).await;
            // The receiver is only gone when exiting.
            let _ = loaded_tx.send((url, image));
        }));
    }

    /// Take in an image fetched by [`Artwork::show`].
    pub fn loaded(&mut self, (url, image): Loaded) {
        let image = match image {
            Ok(image) => image,
            // The image is only decoration: don't bother the user with it.
            Err(err) => return warn!("Failed to fetch album art from {url}: {err:#}"),
        };
        if self.wanted.as_ref() == Some(&url) {
            self.image = Some(self.picker.new_resize_protocol(image.clone()));
        }
        if self.cache.len() == CACHE_SIZE {
            self.cache.pop_front();
        }
        self.cache.push_back((url, image));
    }

    /// The image to draw, if it is loaded.
    pub fn image(&mut self) -> Option<&mut StatefulProtocol> {
        self.image.as_mut()
    }

    /// Whether there is an image to show, loaded or not.
    pub fn is_wanted(&self) -> bool {
        self.wanted.is_some()
    }

    /// The size of the terminal's characters, in pixels.
    pub fn font_size(&self) -> FontSize {
        self.picker.font_size()
    }
}

async fn fetch(client: &reqwest::Client, url: &Url) -> Result<DynamicImage> {
    let bytes = client
        .get(url.clone())
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .context("Request failed")?
        .bytes()
        .await
        .context("Failed to download the image")?;
    tokio::task::spawn_blocking(move || image::load_from_memory(&bytes))
        .await?
        .context("Failed to decode the image")
}
