//! Album art, fetched in the background and drawn with whatever graphics the terminal supports.

use std::{
    collections::VecDeque,
    fs, io,
    path::PathBuf,
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result};
use etcetera::BaseStrategy;
use image::DynamicImage;
use ratatui_image::{FontSize, picker::Picker, protocol::StatefulProtocol};
use sha2::{Digest, Sha256};
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::{debug, warn};
use url::Url;

const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// How many images to keep in memory, so that switching between groups doesn't load them again.
const CACHE_SIZE: usize = 8;

/// A loaded image, or why it couldn't be loaded.
pub type Loaded = (Url, Result<DynamicImage>);

pub struct Artwork {
    picker: Picker,
    client: reqwest::Client,
    disk_cache: Option<DiskCache>,
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
    /// Draw images with `picker`, keeping fetched ones in `disk_cache` if any. Loaded images are
    /// sent to the returned receiver, to be passed to [`Artwork::loaded`].
    pub fn new(
        picker: Picker,
        disk_cache: Option<DiskCache>,
    ) -> (Self, mpsc::UnboundedReceiver<Loaded>) {
        let (loaded_tx, loaded_rx) = mpsc::unbounded_channel();
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .build()
            .expect("the HTTP client's TLS backend can be initialized");
        if let Some(disk_cache) = disk_cache.clone() {
            // In case it was made smaller since the last session.
            tokio::task::spawn_blocking(move || {
                if let Err(err) = disk_cache.prune() {
                    warn!("Failed to clean up {}: {err}", disk_cache.dir.display());
                }
            });
        }
        let artwork = Self {
            picker,
            client,
            disk_cache,
            loaded_tx,
            wanted: None,
            image: None,
            fetch: None,
            cache: VecDeque::new(),
        };
        (artwork, loaded_rx)
    }

    /// Draw images with `picker` from now on.
    pub fn set_picker(&mut self, picker: Picker) {
        self.picker = picker;
        // Prepare the image shown, if any, for the new picker.
        let wanted = self.wanted.take();
        self.show(wanted.as_ref());
    }

    /// Show the image at `url`, loading it unless it was loaded recently.
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
        let client = self.client.clone();
        let disk_cache = self.disk_cache.clone();
        let loaded_tx = self.loaded_tx.clone();
        let url = url.clone();
        self.fetch = Some(tokio::spawn(async move {
            let image = load(&client, disk_cache, &url).await;
            // The receiver is only gone when exiting.
            let _ = loaded_tx.send((url, image));
        }));
    }

    /// Take in an image loaded by [`Artwork::show`].
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

    /// The size of the terminal's cells, in pixels, as far as is known.
    pub fn font_size(&self) -> FontSize {
        self.picker.font_size()
    }
}

/// A disk cache taking up to `max_size_mib` MiB, or none if that is 0. Images are kept in
/// `$XDG_CACHE_HOME/sinuous/artwork`, i.e. `~/.cache/sinuous/artwork` by default, or
/// `%LOCALAPPDATA%\sinuous\artwork` on Windows.
pub fn disk_cache(max_size_mib: u64) -> Option<DiskCache> {
    if max_size_mib == 0 {
        return None;
    }
    let Ok(strategy) = etcetera::choose_base_strategy() else {
        warn!("Failed to find the home directory: not caching album art on disk");
        return None;
    };
    let dir = strategy.cache_dir().join("sinuous").join("artwork");
    Some(DiskCache::new(
        dir,
        max_size_mib.saturating_mul(1024 * 1024),
    ))
}

/// The image at `url`, from the disk cache if it is there, or else fetched and added to it.
async fn load(
    client: &reqwest::Client,
    disk_cache: Option<DiskCache>,
    url: &Url,
) -> Result<DynamicImage> {
    if let Some(disk_cache) = disk_cache.clone() {
        let url = url.clone();
        let cached = tokio::task::spawn_blocking(move || disk_cache.load(&url)).await?;
        if let Some(image) = cached {
            return Ok(image);
        }
    }

    debug!("Fetching {url}");
    let bytes = client
        .get(url.clone())
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .context("Request failed")?
        .bytes()
        .await
        .context("Failed to download the image")?;
    let url = url.clone();
    tokio::task::spawn_blocking(move || {
        let image = image::load_from_memory(&bytes).context("Failed to decode the image")?;
        // Only once decoded, so that whatever was served instead of an image isn't kept.
        if let Some(disk_cache) = disk_cache
            && let Err(err) = disk_cache.store(&url, &bytes)
        {
            warn!("Failed to cache the album art from {url}: {err:#}");
        }
        Ok(image)
    })
    .await?
}

/// Images kept on disk as they were fetched, so that they aren't fetched again in later sessions.
#[derive(Clone)]
pub struct DiskCache {
    dir: PathBuf,
    /// How many bytes the images may take, least recently used ones being removed first.
    max_size: u64,
}

impl DiskCache {
    fn new(dir: PathBuf, max_size: u64) -> Self {
        Self { dir, max_size }
    }

    /// Where the image at `url` is kept: URLs can be too long or contain characters that can't
    /// be in file names, so they are hashed.
    fn path(&self, url: &Url) -> PathBuf {
        let digest = Sha256::digest(url.as_str());
        let name: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        self.dir.join(name)
    }

    /// The image at `url`, if it is cached and can be decoded.
    fn load(&self, url: &Url) -> Option<DynamicImage> {
        let path = self.path(url);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return None,
            Err(err) => {
                warn!("Failed to read {}: {err}", path.display());
                return None;
            }
        };
        match image::load_from_memory(&bytes) {
            Ok(image) => {
                debug!("Loaded {url} from {}", path.display());
                // Mark it as recently used, so that it is the last to be removed.
                let touched = fs::File::options()
                    .write(true)
                    .open(&path)
                    .and_then(|file| file.set_modified(SystemTime::now()));
                if let Err(err) = touched {
                    debug!("Failed to update the time of {}: {err}", path.display());
                }
                Some(image)
            }
            Err(err) => {
                warn!("Removing {}, which can't be decoded: {err}", path.display());
                let _ = fs::remove_file(&path);
                None
            }
        }
    }

    /// Keep `bytes`, the image at `url`, removing the least recently used images if they take
    /// too much space.
    fn store(&self, url: &Url, bytes: &[u8]) -> Result<()> {
        fs::create_dir_all(&self.dir)
            .with_context(|| format!("Failed to create {}", self.dir.display()))?;
        let path = self.path(url);
        // Write it next to where it goes then move it there, so that no one reads it half written.
        let partial = path.with_extension(format!("{}.part", std::process::id()));
        fs::write(&partial, bytes)
            .and_then(|()| fs::rename(&partial, &path))
            .inspect_err(|_| {
                let _ = fs::remove_file(&partial);
            })
            .with_context(|| format!("Failed to write {}", path.display()))?;
        self.prune()
            .with_context(|| format!("Failed to clean up {}", self.dir.display()))
    }

    /// Remove the least recently used images until they take no more than `max_size` bytes.
    fn prune(&self) -> io::Result<()> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            // Nothing was ever cached.
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err),
        };
        let mut files = Vec::new();
        for entry in entries {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_file() {
                files.push((metadata.modified()?, metadata.len(), entry.path()));
            }
        }
        let mut size: u64 = files.iter().map(|(_, len, _)| len).sum();
        if size <= self.max_size {
            return Ok(());
        }
        files.sort_unstable_by_key(|(modified, ..)| *modified);
        for (_, len, path) in files {
            if size <= self.max_size {
                break;
            }
            debug!("Removing {} from the cache", path.display());
            match fs::remove_file(&path) {
                Ok(()) => {}
                // Another instance may have removed it already.
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
            size -= len;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cache in an empty directory, unique to `name`.
    fn disk_cache(name: &str, max_size: u64) -> DiskCache {
        let dir = std::env::temp_dir().join(format!(
            "sinuous-artwork-test-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        DiskCache::new(dir, max_size)
    }

    /// A PNG of a single pixel.
    fn png() -> Vec<u8> {
        let mut bytes = io::Cursor::new(Vec::new());
        DynamicImage::new_rgb8(1, 1)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    fn url(n: usize) -> Url {
        Url::parse(&format!("http://192.168.1.2:1400/getaa?s=1&u={n}")).unwrap()
    }

    fn set_age(cache: &DiskCache, url: &Url, age: Duration) {
        fs::File::options()
            .write(true)
            .open(cache.path(url))
            .unwrap()
            .set_modified(SystemTime::now() - age)
            .unwrap();
    }

    #[test]
    fn loads_what_it_stored() {
        let cache = disk_cache("loads", u64::MAX);
        assert!(cache.load(&url(1)).is_none());

        cache.store(&url(1), &png()).unwrap();

        assert!(cache.load(&url(1)).is_some());
        assert!(cache.load(&url(2)).is_none());
        let names: Vec<_> = fs::read_dir(&cache.dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
        fs::remove_dir_all(&cache.dir).unwrap();
    }

    #[test]
    fn removes_what_cannot_be_decoded() {
        let cache = disk_cache("undecodable", u64::MAX);
        cache.store(&url(1), b"<html>Not found</html>").unwrap();

        assert!(cache.load(&url(1)).is_none());

        assert!(!cache.path(&url(1)).exists());
        fs::remove_dir_all(&cache.dir).unwrap();
    }

    #[test]
    fn removes_least_recently_used_when_too_big() {
        let png = png();
        let len = png.len() as u64;
        let cache = disk_cache("prune", 2 * len);
        cache.store(&url(1), &png).unwrap();
        cache.store(&url(2), &png).unwrap();
        // The first is used after the second.
        set_age(&cache, &url(1), Duration::from_secs(120));
        set_age(&cache, &url(2), Duration::from_secs(60));
        assert!(cache.load(&url(1)).is_some());

        cache.store(&url(3), &png).unwrap();

        assert!(cache.path(&url(1)).exists());
        assert!(!cache.path(&url(2)).exists());
        assert!(cache.path(&url(3)).exists());
        fs::remove_dir_all(&cache.dir).unwrap();
    }

    #[test]
    fn shrinks_when_made_smaller() {
        let png = png();
        let len = png.len() as u64;
        let cache = disk_cache("shrink", u64::MAX);
        cache.store(&url(1), &png).unwrap();
        cache.store(&url(2), &png).unwrap();
        set_age(&cache, &url(1), Duration::from_secs(120));
        set_age(&cache, &url(2), Duration::from_secs(60));

        DiskCache::new(cache.dir.clone(), len).prune().unwrap();

        assert!(!cache.path(&url(1)).exists());
        assert!(cache.path(&url(2)).exists());
        fs::remove_dir_all(&cache.dir).unwrap();
    }

    #[test]
    fn prunes_nothing_when_nothing_was_cached() {
        disk_cache("missing", 0).prune().unwrap();
    }
}
