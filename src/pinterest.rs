use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use tracing::{info, warn};
use url::Url;

use crate::config::{PinterestFetchMode, Settings};

const CLIENT_NAME: &str = "pinterest-saves-to-evernote/0.1";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
struct ApiPage<T> {
    #[serde(default)]
    items: Vec<T>,
    bookmark: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct PinterestBoard {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct PinterestBoardSection {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct PinterestBoardOwner {
    #[serde(default)]
    pub username: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct PinterestImage {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub width: Option<u64>,
    #[serde(default)]
    pub height: Option<u64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl PinterestImage {
    fn area(&self) -> u64 {
        self.width
            .unwrap_or(0)
            .saturating_mul(self.height.unwrap_or(0))
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct PinterestMedia {
    #[serde(default)]
    pub media_type: Option<String>,
    #[serde(default)]
    pub images: BTreeMap<String, PinterestImage>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct PinterestPin {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub link: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub board_id: Option<String>,
    #[serde(default)]
    pub board_section_id: Option<String>,
    #[serde(default)]
    pub board_owner: Option<PinterestBoardOwner>,
    #[serde(default)]
    pub parent_pin_id: Option<String>,
    #[serde(default)]
    pub alt_text: Option<String>,
    #[serde(default)]
    pub creative_type: Option<String>,
    #[serde(default)]
    pub media: Option<PinterestMedia>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl PinterestPin {
    pub fn pin_url(&self) -> String {
        format!("https://www.pinterest.com/pin/{}/", self.id)
    }

    pub fn best_image(&self) -> Option<&PinterestImage> {
        self.media.as_ref().and_then(|media| {
            media
                .images
                .values()
                .filter(|image| {
                    image
                        .url
                        .as_deref()
                        .is_some_and(|url| !url.trim().is_empty())
                })
                .max_by_key(|image| image.area())
        })
    }

    pub fn best_image_url(&self) -> Option<&str> {
        self.best_image()
            .and_then(|image| image.url.as_deref())
            .or_else(|| self.media.as_ref().and_then(|media| media.url.as_deref()))
    }

    pub fn created_at_utc(&self) -> Option<DateTime<Utc>> {
        self.created_at
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Utc))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedPin {
    pub pin: PinterestPin,
    pub board: Option<PinterestBoard>,
    pub section: Option<PinterestBoardSection>,
}

impl SavedPin {
    fn dedupe_key(&self) -> String {
        self.pin.id.clone()
    }
}

pub async fn resolve_access_token(settings: &Settings) -> Result<String> {
    if settings.can_refresh_pinterest_token() {
        let token = refresh_access_token(settings).await?;
        return Ok(token);
    }

    settings
        .pinterest_access_token
        .clone()
        .ok_or_else(|| anyhow!("PINTEREST_ACCESS_TOKEN is required"))
}

pub async fn public_profile_saved_pins(settings: &Settings) -> Result<Vec<SavedPin>> {
    let source = settings
        .public_profile_to_parse_without_api
        .as_deref()
        .ok_or_else(|| anyhow!("PUBLIC_PROFILE_TO_PARSE_WITHOUT_API is required"))?;
    let url = public_profile_url(source)?;

    let client = reqwest::Client::builder()
        .user_agent(CLIENT_NAME)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .context("failed to build Pinterest public profile HTTP client")?;

    info!(
        url = url.as_str(),
        "fetching public Pinterest profile without API"
    );
    let response = client
        .get(url.clone())
        .header(
            reqwest::header::ACCEPT,
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .send()
        .await
        .with_context(|| format!("failed to fetch public Pinterest profile {url}"))?;

    let status = response.status();
    let final_url = response.url().clone();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!(
            "public Pinterest profile fetch returned HTTP {status} for {final_url}: {body}"
        ));
    }

    let html = response.text().await.with_context(|| {
        format!("failed to read public Pinterest profile HTML from {final_url}")
    })?;
    let pins = parse_public_profile_html(&html)?;
    if pins.is_empty() {
        return Err(anyhow!(
            "public Pinterest profile parser found no recent pins in {final_url}; the profile may be private or Pinterest changed the page format"
        ));
    }

    info!(
        pins = pins.len(),
        url = final_url.as_str(),
        "parsed public Pinterest profile pins"
    );
    Ok(pins)
}

async fn refresh_access_token(settings: &Settings) -> Result<String> {
    let client = reqwest::Client::builder()
        .user_agent(CLIENT_NAME)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .context("failed to build Pinterest OAuth HTTP client")?;

    let token_url = endpoint(&settings.pinterest_api_base_url, "/oauth/token")?;
    let response = client
        .post(token_url)
        .basic_auth(
            settings.pinterest_client_id.as_deref().unwrap_or_default(),
            settings.pinterest_client_secret.as_deref(),
        )
        .form(&[
            ("grant_type", "refresh_token"),
            (
                "refresh_token",
                settings
                    .pinterest_refresh_token
                    .as_deref()
                    .unwrap_or_default(),
            ),
            ("scope", settings.pinterest_token_scope.as_str()),
        ])
        .send()
        .await
        .context("failed to refresh Pinterest access token")?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!(
            "Pinterest OAuth refresh failed with HTTP {status}: {body}"
        ));
    }

    let token = response
        .json::<TokenResponse>()
        .await
        .context("failed to parse Pinterest OAuth response")?;

    if token.refresh_token.is_some() {
        warn!(
            "Pinterest returned a refresh token; update the GitHub secret if your app rotates refresh tokens"
        );
    }

    Ok(token.access_token)
}

#[derive(Clone)]
pub struct PinterestClient {
    access_token: String,
    api_base_url: String,
    page_size: usize,
    http: reqwest::Client,
}

impl PinterestClient {
    pub fn new(settings: &Settings, access_token: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(CLIENT_NAME)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("failed to build Pinterest HTTP client")?;
        Ok(Self {
            access_token,
            api_base_url: settings.pinterest_api_base_url.clone(),
            page_size: settings.page_size,
            http,
        })
    }

    pub async fn saved_pins(&self, settings: &Settings) -> Result<Vec<SavedPin>> {
        let pins = match settings.pinterest_fetch_mode {
            PinterestFetchMode::Account => self.account_pins().await?,
            PinterestFetchMode::Boards => self.board_pins(settings).await?,
        };

        let mut deduped = BTreeMap::<String, SavedPin>::new();
        for pin in pins {
            deduped.entry(pin.dedupe_key()).or_insert(pin);
        }
        Ok(deduped.into_values().collect())
    }

    async fn account_pins(&self) -> Result<Vec<SavedPin>> {
        let pins = self.get_paginated::<PinterestPin>("/pins", &[]).await?;
        Ok(pins
            .into_iter()
            .map(|pin| SavedPin {
                pin,
                board: None,
                section: None,
            })
            .collect())
    }

    async fn board_pins(&self, settings: &Settings) -> Result<Vec<SavedPin>> {
        let boards = if settings.pinterest_board_ids.is_empty() {
            self.get_paginated::<PinterestBoard>("/boards", &[]).await?
        } else {
            let mut boards = Vec::new();
            for board_id in &settings.pinterest_board_ids {
                boards.push(
                    self.get_one::<PinterestBoard>(&format!("/boards/{board_id}"))
                        .await?,
                );
            }
            boards
        };

        info!(boards = boards.len(), "fetching Pinterest board pins");
        let mut saved = Vec::new();
        for board in boards {
            let board_path = format!("/boards/{}/pins", board.id);
            let pins = self.get_paginated::<PinterestPin>(&board_path, &[]).await?;
            for pin in pins {
                saved.push(SavedPin {
                    pin,
                    board: Some(board.clone()),
                    section: None,
                });
            }

            if settings.pinterest_include_sections {
                let section_path = format!("/boards/{}/sections", board.id);
                let sections = self
                    .get_paginated::<PinterestBoardSection>(&section_path, &[])
                    .await?;
                for section in sections {
                    let pins_path = format!("/boards/{}/sections/{}/pins", board.id, section.id);
                    let section_pins = self.get_paginated::<PinterestPin>(&pins_path, &[]).await?;
                    for pin in section_pins {
                        saved.push(SavedPin {
                            pin,
                            board: Some(board.clone()),
                            section: Some(section.clone()),
                        });
                    }
                }
            }
        }

        Ok(saved)
    }

    async fn get_one<T>(&self, path: &str) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let url = endpoint(&self.api_base_url, path)?;
        let response = self
            .http
            .get(url)
            .bearer_auth(&self.access_token)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .with_context(|| format!("Pinterest GET {path} failed"))?;

        parse_json_response(response, path).await
    }

    async fn get_paginated<T>(&self, path: &str, query: &[(&str, String)]) -> Result<Vec<T>>
    where
        T: DeserializeOwned,
    {
        let mut items = Vec::new();
        let mut bookmark: Option<String> = None;

        loop {
            let mut url = endpoint(&self.api_base_url, path)?;
            {
                let mut pairs = url.query_pairs_mut();
                pairs.append_pair("page_size", &self.page_size.to_string());
                for (name, value) in query {
                    pairs.append_pair(name, value);
                }
                if let Some(bookmark) = &bookmark {
                    pairs.append_pair("bookmark", bookmark);
                }
            }

            let response = self
                .http
                .get(url)
                .bearer_auth(&self.access_token)
                .header(reqwest::header::ACCEPT, "application/json")
                .send()
                .await
                .with_context(|| format!("Pinterest GET {path} failed"))?;

            let page = parse_json_response::<ApiPage<T>>(response, path).await?;
            items.extend(page.items);
            match page.bookmark {
                Some(next) if !next.trim().is_empty() => bookmark = Some(next),
                _ => break,
            }
        }

        Ok(items)
    }
}

async fn parse_json_response<T>(response: reqwest::Response, path: &str) -> Result<T>
where
    T: DeserializeOwned,
{
    let status = response.status();
    if status == StatusCode::NO_CONTENT {
        return Err(anyhow!("Pinterest GET {path} returned no content"));
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!(
            "Pinterest GET {path} returned HTTP {status}: {body}"
        ));
    }

    response
        .json::<T>()
        .await
        .with_context(|| format!("failed to parse Pinterest response for {path}"))
}

fn endpoint(api_base_url: &str, path: &str) -> Result<Url> {
    let base = api_base_url.trim_end_matches('/');
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    Url::parse(&format!("{base}{path}")).context("invalid Pinterest API URL")
}

fn public_profile_url(raw: &str) -> Result<Url> {
    let source = raw.trim();
    if source.is_empty() {
        return Err(anyhow!(
            "PUBLIC_PROFILE_TO_PARSE_WITHOUT_API must not be empty"
        ));
    }

    if source.starts_with("http://") || source.starts_with("https://") {
        return Url::parse(source).context("invalid PUBLIC_PROFILE_TO_PARSE_WITHOUT_API URL");
    }

    let path = source.trim_matches('/');
    let path = if path.contains('/') {
        path.to_string()
    } else {
        format!("{path}/pins")
    };
    Url::parse(&format!("https://www.pinterest.com/{path}/"))
        .context("invalid PUBLIC_PROFILE_TO_PARSE_WITHOUT_API profile path")
}

fn parse_public_profile_html(html: &str) -> Result<Vec<SavedPin>> {
    let mut deduped = BTreeMap::<String, SavedPin>::new();

    for script in json_ld_scripts(html) {
        let value = match serde_json::from_str::<Value>(script.trim()) {
            Ok(value) => value,
            Err(error) => {
                warn!(error = %error, "failed to parse Pinterest JSON-LD script");
                continue;
            }
        };
        collect_json_ld_pins(&value, &mut deduped);
    }

    Ok(deduped.into_values().collect())
}

fn json_ld_scripts(html: &str) -> Vec<&str> {
    let mut scripts = Vec::new();
    let mut rest = html;

    while let Some(start) = rest.find("<script") {
        rest = &rest[start..];
        let Some(tag_end) = rest.find('>') else {
            break;
        };
        let tag = &rest[..=tag_end];
        let body_start = tag_end + 1;
        let after_tag = &rest[body_start..];
        let Some(script_end) = after_tag.find("</script>") else {
            break;
        };

        if tag.contains("application/ld+json") {
            scripts.push(&after_tag[..script_end]);
        }

        rest = &after_tag[script_end + "</script>".len()..];
    }

    scripts
}

fn collect_json_ld_pins(value: &Value, deduped: &mut BTreeMap<String, SavedPin>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_json_ld_pins(item, deduped);
            }
        }
        Value::Object(object) => {
            if let Some(parts) = object.get("hasPart").and_then(Value::as_array) {
                for part in parts {
                    collect_json_ld_pins(part, deduped);
                }
            }

            if let Some(saved_pin) = parse_json_ld_pin(value) {
                deduped.entry(saved_pin.pin.id.clone()).or_insert(saved_pin);
            }
        }
        _ => {}
    }
}

fn parse_json_ld_pin(value: &Value) -> Option<SavedPin> {
    let pin_url = string_field(value, "url")
        .or_else(|| nested_string_field(value, &["mainEntityOfPage", "url"]))?;
    let id = extract_pin_id_from_url(&pin_url)?;
    let image_url = image_url_from_value(value.get("image"))
        .or_else(|| image_url_from_value(value.get("thumbnailUrl")));
    let media = image_url.map(|url| PinterestMedia {
        media_type: Some("image".to_string()),
        images: BTreeMap::from([(
            "public_profile".to_string(),
            PinterestImage {
                url: Some(url),
                width: None,
                height: None,
                extra: Map::new(),
            },
        )]),
        url: None,
        extra: Map::new(),
    });
    let mut extra = Map::new();
    if let Some(author) = author_name(value.get("author")) {
        extra.insert("public_author".to_string(), Value::String(author));
    }

    Some(SavedPin {
        pin: PinterestPin {
            id,
            title: string_field(value, "headline").or_else(|| string_field(value, "name")),
            description: string_field(value, "description"),
            link: None,
            created_at: string_field(value, "datePublished"),
            board_id: None,
            board_section_id: None,
            board_owner: None,
            parent_pin_id: None,
            alt_text: None,
            creative_type: Some("public_profile_json_ld".to_string()),
            media,
            extra,
        },
        board: None,
        section: None,
    })
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn nested_string_field(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn image_url_from_value(value: Option<&Value>) -> Option<String> {
    let value = value?;
    match value {
        Value::String(url) => clean_string(url),
        Value::Object(_) => string_field(value, "url")
            .or_else(|| string_field(value, "contentUrl"))
            .or_else(|| string_field(value, "thumbnailUrl")),
        Value::Array(values) => values
            .iter()
            .find_map(|value| image_url_from_value(Some(value))),
        _ => None,
    }
}

fn author_name(value: Option<&Value>) -> Option<String> {
    let value = value?;
    match value {
        Value::String(name) => clean_string(name),
        Value::Object(_) => string_field(value, "name")
            .or_else(|| string_field(value, "alternateName"))
            .or_else(|| string_field(value, "url")),
        Value::Array(values) => values.iter().find_map(|value| author_name(Some(value))),
        _ => None,
    }
}

fn clean_string(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn extract_pin_id_from_url(url: &str) -> Option<String> {
    let marker = "/pin/";
    let after_pin = url
        .find(marker)
        .map(|index| &url[index + marker.len()..])
        .unwrap_or(url);
    let segment = after_pin.split(['/', '?', '#']).next().unwrap_or(after_pin);
    let mut current = String::new();
    let mut last = None;

    for character in segment.chars() {
        if character.is_ascii_digit() {
            current.push(character);
        } else if !current.is_empty() {
            last = Some(std::mem::take(&mut current));
        }
    }

    if !current.is_empty() {
        last = Some(current);
    }

    last
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chooses_largest_image() {
        let pin = PinterestPin {
            id: "1".to_string(),
            title: None,
            description: None,
            link: None,
            created_at: None,
            board_id: None,
            board_section_id: None,
            board_owner: None,
            parent_pin_id: None,
            alt_text: None,
            creative_type: None,
            media: Some(PinterestMedia {
                media_type: Some("image".to_string()),
                images: BTreeMap::from([
                    (
                        "small".to_string(),
                        PinterestImage {
                            url: Some("https://example.com/s.jpg".to_string()),
                            width: Some(100),
                            height: Some(100),
                            extra: Map::new(),
                        },
                    ),
                    (
                        "large".to_string(),
                        PinterestImage {
                            url: Some("https://example.com/l.jpg".to_string()),
                            width: Some(800),
                            height: Some(600),
                            extra: Map::new(),
                        },
                    ),
                ]),
                url: None,
                extra: Map::new(),
            }),
            extra: Map::new(),
        };

        assert_eq!(pin.best_image_url(), Some("https://example.com/l.jpg"));
    }

    #[test]
    fn extracts_pin_id_from_plain_and_slug_urls() {
        assert_eq!(
            extract_pin_id_from_url("https://www.pinterest.com/pin/332633122505892666/"),
            Some("332633122505892666".to_string())
        );
        assert_eq!(
            extract_pin_id_from_url(
                "https://uk.pinterest.com/pin/example-title-with-90s--218213544442262202/"
            ),
            Some("218213544442262202".to_string())
        );
    }

    #[test]
    fn parses_public_profile_json_ld_pins() {
        let html = r#"
            <html>
              <script data-test-id="profile-snippet" type="application/ld+json">
                {
                  "@context": "https://schema.org/",
                  "@type": "ProfilePage",
                  "hasPart": [
                    {
                      "@type": "SocialMediaPosting",
                      "author": {"@type": "Person", "name": "example-author"},
                      "headline": "Example pin title",
                      "description": "Example description",
                      "image": "https://i.pinimg.com/736x/example.jpg",
                      "url": "https://www.pinterest.com/pin/example-title--123456789/",
                      "datePublished": "2026-06-14T20:32:17.000Z"
                    }
                  ]
                }
              </script>
            </html>
        "#;

        let pins = parse_public_profile_html(html).unwrap();

        assert_eq!(pins.len(), 1);
        assert_eq!(pins[0].pin.id, "123456789");
        assert_eq!(pins[0].pin.title.as_deref(), Some("Example pin title"));
        assert_eq!(
            pins[0].pin.best_image_url(),
            Some("https://i.pinimg.com/736x/example.jpg")
        );
        assert_eq!(
            pins[0]
                .pin
                .extra
                .get("public_author")
                .and_then(Value::as_str),
            Some("example-author")
        );
    }
}
