use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use url::Url;

const CLIENT_NAME: &str = "pinterest-saves-to-evernote/0.1";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Debug)]
pub struct DownloadedImage {
    pub source_url: String,
    pub bytes: Vec<u8>,
    pub mime_type: String,
    pub hash: Vec<u8>,
    pub hash_hex: String,
    pub file_name: String,
}

#[derive(Clone)]
pub struct ImageDownloader {
    client: reqwest::Client,
    max_bytes: u64,
}

impl ImageDownloader {
    pub fn new(max_bytes: u64) -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(CLIENT_NAME)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("failed to build image HTTP client")?;
        Ok(Self { client, max_bytes })
    }

    pub async fn download(&self, image_url: &str, pin_id: &str) -> Result<DownloadedImage> {
        let response = self
            .client
            .get(image_url)
            .send()
            .await
            .with_context(|| format!("failed to download Pinterest image {image_url}"))?;

        let status = response.status();
        if !status.is_success() {
            return Err(anyhow!(
                "Pinterest image download returned HTTP {status}: {image_url}"
            ));
        }

        // Check Content-Length before reading the body, then check the decoded body
        // size again because servers may omit or misreport the header.
        if let Some(content_length) = response.content_length()
            && content_length > self.max_bytes
        {
            return Err(anyhow!(
                "Pinterest image is too large: {content_length} bytes exceeds MAX_IMAGE_BYTES={}",
                self.max_bytes
            ));
        }

        let raw_content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(normalize_content_type);
        let mime_type = match raw_content_type {
            Some(value) if value.starts_with("image/") => value,
            Some(value) => {
                return Err(anyhow!(
                    "Pinterest image URL returned non-image content type {value}: {image_url}"
                ));
            }
            None => "image/jpeg".to_string(),
        };

        let bytes = response
            .bytes()
            .await
            .context("failed to read Pinterest image response body")?
            .to_vec();
        if bytes.len() as u64 > self.max_bytes {
            return Err(anyhow!(
                "Pinterest image is too large: {} bytes exceeds MAX_IMAGE_BYTES={}",
                bytes.len(),
                self.max_bytes
            ));
        }

        // Evernote resources are referenced from ENML by MD5 digest.
        let digest = md5::compute(&bytes);
        let hash = digest.0.to_vec();
        let hash_hex = format!("{digest:x}");
        let file_name = file_name_for(image_url, pin_id, &mime_type);

        Ok(DownloadedImage {
            source_url: image_url.to_string(),
            bytes,
            mime_type,
            hash,
            hash_hex,
            file_name,
        })
    }
}

fn normalize_content_type(raw: &str) -> String {
    raw.split(';')
        .next()
        .unwrap_or(raw)
        .trim()
        .to_ascii_lowercase()
}

fn file_name_for(image_url: &str, pin_id: &str, mime_type: &str) -> String {
    Url::parse(image_url)
        .ok()
        .and_then(|url| {
            url.path_segments()
                .and_then(|mut segments| segments.next_back())
                .map(str::to_string)
        })
        .filter(|name| !name.trim().is_empty() && name.contains('.'))
        .unwrap_or_else(|| format!("pinterest-{pin_id}.{}", extension_for_mime(mime_type)))
}

fn extension_for_mime(mime_type: &str) -> &'static str {
    match mime_type {
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/svg+xml" => "svg",
        _ => "jpg",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_content_type() {
        assert_eq!(
            normalize_content_type("Image/JPEG; charset=binary"),
            "image/jpeg"
        );
        assert_eq!(normalize_content_type(" image/PNG "), "image/png");
    }

    #[test]
    fn derives_file_name() {
        assert_eq!(
            file_name_for(
                "https://example.com/path/image.webp?x=1",
                "123",
                "image/jpeg"
            ),
            "image.webp"
        );
        assert_eq!(
            file_name_for("https://example.com/path/", "123", "image/png"),
            "pinterest-123.png"
        );
        assert_eq!(
            file_name_for("not a url", "123", "image/svg+xml"),
            "pinterest-123.svg"
        );
    }

    #[test]
    fn maps_common_image_extensions() {
        assert_eq!(extension_for_mime("image/png"), "png");
        assert_eq!(extension_for_mime("image/gif"), "gif");
        assert_eq!(extension_for_mime("image/webp"), "webp");
        assert_eq!(extension_for_mime("image/bmp"), "bmp");
        assert_eq!(extension_for_mime("image/svg+xml"), "svg");
        assert_eq!(extension_for_mime("image/avif"), "jpg");
    }
}
