use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use reqwest::header::{ACCEPT, COOKIE, HeaderMap, HeaderValue, REFERER, SET_COOKIE, USER_AGENT};
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
        // Pinterest usually returns several named image sizes. Prefer the largest
        // available image when attaching media to Evernote.
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
            .and_then(|value| {
                DateTime::parse_from_rfc3339(value)
                    .or_else(|_| DateTime::parse_from_rfc2822(value))
                    .ok()
            })
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicPinComment {
    pub id: Option<String>,
    pub text: String,
    pub created_at: Option<String>,
    pub parent_comment_id: Option<String>,
    pub user_id: Option<String>,
    pub user_username: Option<String>,
    pub user_full_name: Option<String>,
    pub user_url: Option<String>,
    pub reply_count: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicPinComments {
    pub total_count: Option<u64>,
    pub comments: Vec<PublicPinComment>,
}

impl PublicPinComments {
    pub fn attach_to_extra(&self, extra: &mut Map<String, Value>) {
        // Notes are rendered from SavedPin only, so scraped comment data is carried
        // through the same `extra` map used for fields not present in the official API.
        if let Some(total_count) = self.total_count {
            extra.insert("public_comment_count".to_string(), Value::from(total_count));
        }

        if self.comments.is_empty() {
            return;
        }

        let comments = self
            .comments
            .iter()
            .map(|comment| {
                let mut value = Map::new();
                if let Some(id) = &comment.id {
                    value.insert("id".to_string(), Value::String(id.clone()));
                }
                value.insert("text".to_string(), Value::String(comment.text.clone()));
                if let Some(created_at) = &comment.created_at {
                    value.insert("created_at".to_string(), Value::String(created_at.clone()));
                }
                if let Some(parent_comment_id) = &comment.parent_comment_id {
                    value.insert(
                        "parent_comment_id".to_string(),
                        Value::String(parent_comment_id.clone()),
                    );
                }
                if let Some(user_id) = &comment.user_id {
                    value.insert("user_id".to_string(), Value::String(user_id.clone()));
                }
                if let Some(username) = &comment.user_username {
                    value.insert("user_username".to_string(), Value::String(username.clone()));
                }
                if let Some(full_name) = &comment.user_full_name {
                    value.insert(
                        "user_full_name".to_string(),
                        Value::String(full_name.clone()),
                    );
                }
                if let Some(url) = &comment.user_url {
                    value.insert("user_url".to_string(), Value::String(url.clone()));
                }
                if let Some(reply_count) = comment.reply_count {
                    value.insert("reply_count".to_string(), Value::from(reply_count));
                }
                Value::Object(value)
            })
            .collect::<Vec<_>>();
        extra.insert("public_comments".to_string(), Value::Array(comments));
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AggregatedPinData {
    entity_id: String,
    comment_count: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PublicProfilePinsPage {
    pins: Vec<SavedPin>,
    next_bookmark: Option<String>,
    diagnostics: PublicProfileParseDiagnostics,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PublicProfileParseDiagnostics {
    user_pins_resource_marker_found: bool,
    user_pins_resource_json_found: bool,
    user_pins_resource_parse_error: Option<String>,
    user_pins_resource_objects: usize,
    user_pins_resource_data_arrays: usize,
    user_pins_resource_pins: usize,
    json_ld_scripts: usize,
    json_ld_parse_errors: usize,
    json_ld_pins: usize,
}

pub async fn resolve_access_token(settings: &Settings) -> Result<String> {
    // Prefer refresh-token based auth when configured. It lets scheduled CI runs
    // continue after a short-lived Pinterest access token expires.
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
    let configured_cookie_header =
        configured_pinterest_cookie_header(settings.pinterest_cookie.as_deref())?;
    let mut page_request = client.get(url.clone()).header(
        reqwest::header::ACCEPT,
        "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
    );
    if let Some(cookie_header) = &configured_cookie_header {
        page_request = page_request.header(COOKIE, cookie_header.clone());
    }
    let response = page_request
        .send()
        .await
        .with_context(|| format!("failed to fetch public Pinterest profile {url}"))?;

    let status = response.status();
    let final_url = response.url().clone();
    let cookie_header = merge_cookie_headers(
        configured_cookie_header.as_ref(),
        response_cookie_header(response.headers()).as_ref(),
    );
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!(
            "public Pinterest profile fetch returned HTTP {status} for {final_url}: {body}"
        ));
    }

    let html = response.text().await.with_context(|| {
        format!("failed to read public Pinterest profile HTML from {final_url}")
    })?;
    // The first page is real HTML. It normally contains Pinterest's embedded
    // UserPinsResource state; JSON-LD is kept as a weaker fallback.
    let mut page = parse_public_profile_html(&html)?;
    info!(
        user_pins_resource_marker_found = page.diagnostics.user_pins_resource_marker_found,
        user_pins_resource_json_found = page.diagnostics.user_pins_resource_json_found,
        user_pins_resource_parse_error = page.diagnostics.user_pins_resource_parse_error.as_deref(),
        user_pins_resource_objects = page.diagnostics.user_pins_resource_objects,
        user_pins_resource_data_arrays = page.diagnostics.user_pins_resource_data_arrays,
        user_pins_resource_pins = page.diagnostics.user_pins_resource_pins,
        json_ld_scripts = page.diagnostics.json_ld_scripts,
        json_ld_parse_errors = page.diagnostics.json_ld_parse_errors,
        json_ld_pins = page.diagnostics.json_ld_pins,
        "parsed public Pinterest profile HTML stages"
    );
    let username = public_profile_username(&final_url)
        .or_else(|| public_profile_username(&url))
        .ok_or_else(|| anyhow!("public Pinterest profile URL has no username: {final_url}"))?;
    if page.pins.is_empty() {
        return Err(anyhow!(
            "public Pinterest profile parser found no recent pins in {final_url}; parser diagnostics: {}; the profile may be private or Pinterest changed the page format",
            profile_parse_diagnostics(&page.diagnostics)
        ));
    }

    let mut pins = std::mem::take(&mut page.pins);
    let mut seen = pins
        .iter()
        .map(|saved| saved.pin.id.clone())
        .collect::<BTreeSet<_>>();
    let source_url = format!("/{username}/");
    let referer = final_url.to_string();

    // Later pages use the same resource endpoint Pinterest's web app calls.
    // Reusing response cookies keeps the request close to the browser flow and
    // avoids needing a logged-in session.
    for page_number in 2..=settings.public_profile_max_pages {
        let Some(bookmark) = page.next_bookmark.take() else {
            break;
        };
        match fetch_public_profile_pin_page(
            &client,
            &username,
            &source_url,
            &referer,
            &bookmark,
            cookie_header.clone(),
        )
        .await
        {
            Ok(next_page) => {
                let parsed_pins = next_page.pins.len();
                let next_bookmark = next_page.next_bookmark;
                for saved in next_page.pins {
                    append_unique_pin(&mut pins, &mut seen, saved);
                }
                info!(
                    page = page_number,
                    pins = parsed_pins,
                    total_pins = pins.len(),
                    "parsed paginated public Pinterest profile pins"
                );
                if parsed_pins == 0 {
                    break;
                }
                page.next_bookmark = next_bookmark;
            }
            Err(error) => {
                warn!(
                    page = page_number,
                    error = %error,
                    "failed to fetch paginated public Pinterest profile pins; continuing with pins already parsed"
                );
                break;
            }
        }
    }

    info!(
        pins = pins.len(),
        url = final_url.as_str(),
        "parsed public Pinterest profile pins"
    );
    Ok(pins)
}

async fn fetch_public_profile_pin_page(
    client: &reqwest::Client,
    username: &str,
    source_url: &str,
    referer: &str,
    bookmark: &str,
    cookie_header: Option<HeaderValue>,
) -> Result<PublicProfilePinsPage> {
    // This is an internal Pinterest web resource, not the documented API. Keep
    // the options and headers close to the browser request shape because this
    // endpoint is more brittle than the official API.
    let data = serde_json::json!({
        "options": {
            "add_vase": true,
            "field_set_key": "mobile_grid_item",
            "is_own_profile_pins": false,
            "username": username,
            "bookmarks": [bookmark],
        },
        "context": {}
    })
    .to_string();
    let resource_url = Url::parse("https://www.pinterest.com/resource/UserPinsResource/get/")
        .context("invalid Pinterest user pins resource URL")?;

    let mut request = client
        .get(resource_url)
        .header(ACCEPT, "application/json, text/javascript, */*; q=0.01")
        .header("X-Requested-With", "XMLHttpRequest")
        .header("X-Pinterest-AppState", "active")
        .header("X-Pinterest-PWS-Handler", "www/[username]")
        .header(USER_AGENT, CLIENT_NAME)
        .header(REFERER, referer)
        .query(&[("source_url", source_url), ("data", data.as_str())]);
    if let Some(cookie_header) = cookie_header {
        request = request.header(COOKIE, cookie_header);
    }

    let response = request
        .send()
        .await
        .with_context(|| {
            format!(
                "public Pinterest profile pagination fetch stage failed for username={username}, bookmark={bookmark}"
            )
        })?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!(
            "public Pinterest profile pagination HTTP stage failed for username={username}, bookmark={bookmark}: UserPinsResource returned HTTP {status}: {body}"
        ));
    }

    let value = response
        .json::<Value>()
        .await
        .with_context(|| {
            format!(
                "public Pinterest profile pagination JSON stage failed for username={username}, bookmark={bookmark}"
            )
        })?;
    parse_public_profile_pin_page_response(&value).with_context(|| {
        format!(
            "public Pinterest profile pagination resource_response stage failed for username={username}, bookmark={bookmark}"
        )
    })
}

pub async fn scrape_public_pin_comments(
    pin_id: &str,
    max_comments: usize,
    pinterest_cookie: Option<&str>,
) -> Result<PublicPinComments> {
    let pin_url = format!("https://www.pinterest.com/pin/{pin_id}/");
    let comments_url = format!("https://www.pinterest.com/pin/{pin_id}/comments/");
    let source_url = format!("/pin/{pin_id}/comments/");

    let client = reqwest::Client::builder()
        .user_agent(CLIENT_NAME)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .context("failed to build Pinterest public comments HTTP client")?;

    let configured_cookie_header = configured_pinterest_cookie_header(pinterest_cookie)?;
    let mut page_request = client.get(&comments_url).header(
        ACCEPT,
        "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
    );
    if let Some(cookie_header) = &configured_cookie_header {
        page_request = page_request.header(COOKIE, cookie_header.clone());
    }
    let page_response = page_request.send().await.with_context(|| {
        format!("failed to fetch public Pinterest comments page {comments_url}")
    })?;

    let status = page_response.status();
    let final_url = page_response.url().clone();
    let cookie_header = merge_cookie_headers(
        configured_cookie_header.as_ref(),
        response_cookie_header(page_response.headers()).as_ref(),
    );
    if !status.is_success() {
        let body = page_response.text().await.unwrap_or_default();
        return Err(anyhow!(
            "public Pinterest comments page returned HTTP {status} for {final_url}: {body}"
        ));
    }

    let html = page_response.text().await.with_context(|| {
        format!("failed to read public Pinterest comments page HTML from {final_url}")
    })?;
    // The comments resource needs an aggregated pin entity id, which is not the
    // same as the visible pin id. Pinterest embeds it in the public comments page.
    let aggregated = parse_aggregated_pin_data_from_html(&html).ok_or_else(|| {
        let marker_found = html.contains("\"aggregatedPinData\":");
        anyhow!(
            "public Pinterest comments entity-id stage failed for pin {pin_id} at {final_url}: aggregatedPinData marker found={marker_found}, but entityId/comment metadata could not be parsed"
        )
    })?;
    let mut summary = PublicPinComments {
        total_count: aggregated.comment_count,
        comments: Vec::new(),
    };

    if aggregated.comment_count == Some(0) {
        return Ok(summary);
    }

    // The public response may include only numeric user ids for commenters.
    // Rendering code intentionally ignores those ids unless a username or URL is
    // also present, because the public scraper has no reliable id-to-profile map.
    let data = serde_json::json!({
        "options": {
            "url": format!("/v3/aggregated_pin_data/{}/comments/", aggregated.entity_id),
            "data": {}
        },
        "context": {}
    })
    .to_string();
    let resource_url = Url::parse("https://www.pinterest.com/resource/ApiResource/get/")
        .context("invalid Pinterest comments resource URL")?;

    let mut request = client
        .get(resource_url)
        .header(ACCEPT, "application/json, text/javascript, */*; q=0.01")
        .header("X-Requested-With", "XMLHttpRequest")
        .header("X-Pinterest-AppState", "active")
        .header("X-Pinterest-PWS-Handler", "www/pin/[id]/comments")
        .header(USER_AGENT, CLIENT_NAME)
        .header(REFERER, &comments_url)
        .query(&[("source_url", source_url.as_str()), ("data", data.as_str())]);
    if let Some(cookie_header) = &cookie_header {
        request = request.header(COOKIE, cookie_header.clone());
    }

    let response = request
        .send()
        .await
        .with_context(|| {
            format!(
                "public Pinterest comments resource fetch stage failed for {pin_url}, aggregated_entity_id={}",
                aggregated.entity_id
            )
        })?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!(
            "public Pinterest comments resource HTTP stage failed for {pin_url}, aggregated_entity_id={}: ApiResource returned HTTP {status}: {body}",
            aggregated.entity_id
        ));
    }

    let value = response
        .json::<Value>()
        .await
        .with_context(|| {
            format!(
                "public Pinterest comments resource JSON stage failed for {pin_url}, aggregated_entity_id={}",
                aggregated.entity_id
            )
        })?;
    let top_level_comments =
        parse_public_pin_comments_response(&value, max_comments).with_context(|| {
            format!(
                "public Pinterest comments resource_response stage failed for {pin_url}, aggregated_entity_id={}",
                aggregated.entity_id
            )
        })?;
    summary.comments = fetch_public_pin_comment_replies_for_top_level(
        &client,
        pin_id,
        top_level_comments,
        max_comments,
        cookie_header,
    )
    .await;
    info!(
        pin_id = pin_id,
        comments = summary.comments.len(),
        total_comments = summary.total_count,
        "scraped public Pinterest comments"
    );
    Ok(summary)
}

async fn fetch_public_pin_comment_replies_for_top_level(
    client: &reqwest::Client,
    pin_id: &str,
    top_level_comments: Vec<PublicPinComment>,
    max_comments: usize,
    cookie_header: Option<HeaderValue>,
) -> Vec<PublicPinComment> {
    let mut comments = Vec::new();
    for comment in top_level_comments {
        if comments.len() >= max_comments {
            break;
        }
        let parent_comment_id = comment.id.clone();
        let reply_count = comment.reply_count.unwrap_or(0);
        comments.push(comment);
        let remaining = max_comments.saturating_sub(comments.len());
        if remaining == 0 {
            break;
        }
        let Some(parent_comment_id) = parent_comment_id else {
            continue;
        };
        if reply_count == 0 {
            continue;
        }

        match fetch_public_pin_comment_replies(
            client,
            pin_id,
            &parent_comment_id,
            remaining,
            cookie_header.clone(),
        )
        .await
        {
            Ok(replies) => comments.extend(replies),
            Err(error) => warn!(
                pin_id = pin_id,
                parent_comment_id = parent_comment_id,
                error = %error,
                "failed to scrape public Pinterest comment replies; continuing without replies"
            ),
        }
    }
    comments
}

async fn fetch_public_pin_comment_replies(
    client: &reqwest::Client,
    pin_id: &str,
    parent_comment_id: &str,
    max_replies: usize,
    cookie_header: Option<HeaderValue>,
) -> Result<Vec<PublicPinComment>> {
    let comments_url = format!("https://www.pinterest.com/pin/{pin_id}/comments/");
    let comment_url =
        format!("https://www.pinterest.com/pin/{pin_id}/comments/{parent_comment_id}/");
    let source_url = format!("/pin/{pin_id}/comments/{parent_comment_id}/");
    let data = serde_json::json!({
        "options": {
            "url": format!("/v3/aggregated_comments/{parent_comment_id}/replies/"),
            "data": {}
        },
        "context": {}
    })
    .to_string();
    let resource_url = Url::parse("https://www.pinterest.com/resource/ApiResource/get/")
        .context("invalid Pinterest comment replies resource URL")?;

    let mut request = client
        .get(resource_url)
        .header(ACCEPT, "application/json, text/javascript, */*; q=0.01")
        .header("X-Requested-With", "XMLHttpRequest")
        .header("X-Pinterest-AppState", "active")
        .header(
            "X-Pinterest-PWS-Handler",
            "www/pin/[id]/comments/[comment_id]",
        )
        .header(USER_AGENT, CLIENT_NAME)
        .header(REFERER, &comment_url)
        .query(&[("source_url", source_url.as_str()), ("data", data.as_str())]);
    if let Some(cookie_header) = cookie_header {
        request = request.header(COOKIE, cookie_header);
    }

    let response = request.send().await.with_context(|| {
        format!(
            "public Pinterest comment replies fetch stage failed for pin {pin_id}, parent_comment_id={parent_comment_id}"
        )
    })?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!(
            "public Pinterest comment replies HTTP stage failed for pin {pin_id}, parent_comment_id={parent_comment_id}: ApiResource returned HTTP {status}: {body}"
        ));
    }

    let value = response.json::<Value>().await.with_context(|| {
        format!(
            "public Pinterest comment replies JSON stage failed for pin {pin_id}, parent_comment_id={parent_comment_id}"
        )
    })?;
    parse_public_pin_comment_replies_response(&value, max_replies, parent_comment_id)
        .with_context(|| {
            format!(
                "public Pinterest comment replies resource_response stage failed for pin {pin_id}, parent_comment_id={parent_comment_id}, referer={comments_url}"
            )
        })
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

        // Board and section scans can return the same pin multiple times.
        // The pin id is the stable idempotency key used by both sync state and notes.
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

fn public_profile_username(url: &Url) -> Option<String> {
    url.path_segments()?
        .find(|segment| !segment.is_empty())
        .map(ToOwned::to_owned)
}

fn parse_public_profile_html(html: &str) -> Result<PublicProfilePinsPage> {
    let mut pins = Vec::new();
    let mut seen = BTreeSet::new();
    let mut diagnostics = PublicProfileParseDiagnostics::default();
    let next_bookmark =
        parse_user_pins_resource_from_html(html, &mut pins, &mut seen, &mut diagnostics);

    if pins.is_empty() {
        // JSON-LD has less metadata and usually fewer pins, but it is useful when
        // Pinterest changes the embedded application state while keeping SEO data.
        let before_json_ld = pins.len();
        for script in json_ld_scripts(html) {
            diagnostics.json_ld_scripts += 1;
            let value = match serde_json::from_str::<Value>(script.trim()) {
                Ok(value) => value,
                Err(error) => {
                    diagnostics.json_ld_parse_errors += 1;
                    warn!(error = %error, "failed to parse Pinterest JSON-LD script");
                    continue;
                }
            };
            collect_json_ld_pins(&value, &mut pins, &mut seen);
        }
        diagnostics.json_ld_pins = pins.len().saturating_sub(before_json_ld);
    }

    Ok(PublicProfilePinsPage {
        pins,
        next_bookmark,
        diagnostics,
    })
}

fn parse_user_pins_resource_from_html(
    html: &str,
    pins: &mut Vec<SavedPin>,
    seen: &mut BTreeSet<String>,
    diagnostics: &mut PublicProfileParseDiagnostics,
) -> Option<String> {
    // Avoid scraping arbitrary JavaScript with regexes. We locate the resource
    // marker, then extract the balanced JSON object that starts immediately after it.
    let marker = "\"UserPinsResource\":";
    let Some(start) = html.find(marker).map(|start| start + marker.len()) else {
        return None;
    };
    diagnostics.user_pins_resource_marker_found = true;
    let Some(object) = json_object_prefix(&html[start..]) else {
        return None;
    };
    diagnostics.user_pins_resource_json_found = true;
    let resources = match serde_json::from_str::<Value>(object) {
        Ok(value) => value,
        Err(error) => {
            diagnostics.user_pins_resource_parse_error = Some(error.to_string());
            warn!(error = %error, "failed to parse Pinterest UserPinsResource state");
            return None;
        }
    };

    let mut next_bookmark = None;
    let Some(resources) = resources.as_object() else {
        diagnostics.user_pins_resource_parse_error =
            Some("UserPinsResource state is not a JSON object".to_string());
        return None;
    };
    diagnostics.user_pins_resource_objects = resources.len();
    for resource in resources.values() {
        let Some(data) = resource.get("data").and_then(Value::as_array) else {
            continue;
        };
        diagnostics.user_pins_resource_data_arrays += 1;
        let mut parsed = 0_usize;
        for pin in data.iter().filter_map(parse_public_profile_resource_pin) {
            parsed += 1;
            append_unique_pin(pins, seen, pin);
        }
        diagnostics.user_pins_resource_pins += parsed;
        if parsed > 0 && next_bookmark.is_none() {
            next_bookmark = public_profile_next_bookmark(resource);
        }
    }

    next_bookmark
}

fn parse_public_profile_pin_page_response(value: &Value) -> Result<PublicProfilePinsPage> {
    let response = value
        .get("resource_response")
        .ok_or_else(|| anyhow!("Pinterest user pins response has no resource_response"))?;
    if let Some(error) = response.get("error") {
        return Err(anyhow!(
            "Pinterest user pins response error: {}",
            pinterest_error_message(error)
        ));
    }

    let data = response
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Pinterest user pins response has no data array"))?;

    let mut pins = Vec::new();
    let mut seen = BTreeSet::new();
    for pin in data.iter().filter_map(parse_public_profile_resource_pin) {
        append_unique_pin(&mut pins, &mut seen, pin);
    }

    Ok(PublicProfilePinsPage {
        pins,
        next_bookmark: public_profile_next_bookmark(response),
        diagnostics: PublicProfileParseDiagnostics::default(),
    })
}

fn profile_parse_diagnostics(diagnostics: &PublicProfileParseDiagnostics) -> String {
    let user_pins_error = diagnostics
        .user_pins_resource_parse_error
        .as_deref()
        .unwrap_or("none");
    format!(
        "UserPinsResource marker_found={}, json_found={}, parse_error={user_pins_error:?}, objects={}, data_arrays={}, pins={}; JSON-LD scripts={}, parse_errors={}, pins={}",
        diagnostics.user_pins_resource_marker_found,
        diagnostics.user_pins_resource_json_found,
        diagnostics.user_pins_resource_objects,
        diagnostics.user_pins_resource_data_arrays,
        diagnostics.user_pins_resource_pins,
        diagnostics.json_ld_scripts,
        diagnostics.json_ld_parse_errors,
        diagnostics.json_ld_pins,
    )
}

fn public_profile_next_bookmark(value: &Value) -> Option<String> {
    string_field(value, "nextBookmark")
        .or_else(|| string_field(value, "bookmark"))
        .filter(|bookmark| bookmark != "-end-")
}

fn append_unique_pin(pins: &mut Vec<SavedPin>, seen: &mut BTreeSet<String>, saved_pin: SavedPin) {
    if seen.insert(saved_pin.pin.id.clone()) {
        pins.push(saved_pin);
    }
}

fn parse_public_profile_resource_pin(value: &Value) -> Option<SavedPin> {
    let id = string_field(value, "id")?;
    let board = parse_public_profile_board(value.get("board"));
    let board_id = board
        .as_ref()
        .map(|board| board.id.clone())
        .or_else(|| nested_string_field(value, &["board", "id"]));
    let board_owner =
        nested_string_field(value, &["pinner", "username"]).map(|username| PinterestBoardOwner {
            username: Some(username),
            extra: Map::new(),
        });
    // Public profile data uses a different schema from API v5. Normalize the
    // fields we care about into PinterestPin and keep supplemental values in extra.
    let mut extra = Map::new();
    if let Some(author) = nested_string_field(value, &["origin_pinner", "full_name"])
        .or_else(|| nested_string_field(value, &["origin_pinner", "username"]))
        .or_else(|| nested_string_field(value, &["native_creator", "full_name"]))
        .or_else(|| nested_string_field(value, &["native_creator", "username"]))
    {
        extra.insert("public_author".to_string(), Value::String(author));
    }
    if let Some(username) = nested_string_field(value, &["origin_pinner", "username"]) {
        extra.insert(
            "public_author_username".to_string(),
            Value::String(username),
        );
    }
    if let Some(domain) =
        string_field(value, "domain").or_else(|| string_field(value, "link_domain"))
    {
        extra.insert("public_source_domain".to_string(), Value::String(domain));
    }

    Some(SavedPin {
        pin: PinterestPin {
            id,
            title: string_field(value, "grid_title")
                .or_else(|| string_field(value, "seo_title"))
                .or_else(|| string_field(value, "title")),
            description: string_field(value, "description"),
            link: string_field(value, "link")
                .or_else(|| string_field(value, "tracked_link"))
                .or_else(|| string_field(value, "utm_link")),
            created_at: string_field(value, "created_at").map(normalize_pinterest_datetime),
            board_id,
            board_section_id: None,
            board_owner,
            parent_pin_id: nested_string_field(value, &["pin_join", "canonical_pin_id"]),
            alt_text: string_field(value, "alt_text")
                .or_else(|| string_field(value, "auto_alt_text")),
            creative_type: Some("public_profile_resource".to_string()),
            media: public_profile_media(value),
            extra,
        },
        board,
        section: None,
    })
}

fn parse_public_profile_board(value: Option<&Value>) -> Option<PinterestBoard> {
    let value = value?;
    let id = string_field(value, "id")?;
    let extra = value.as_object().cloned().unwrap_or_default();
    Some(PinterestBoard {
        id,
        name: string_field(value, "name"),
        extra,
    })
}

fn public_profile_media(value: &Value) -> Option<PinterestMedia> {
    let images = value.get("images")?.as_object()?;
    let mut parsed = BTreeMap::new();

    for (key, image) in images {
        let Some(url) = string_field(image, "url") else {
            continue;
        };
        parsed.insert(
            key.clone(),
            PinterestImage {
                url: Some(url),
                width: image.get("width").and_then(Value::as_u64),
                height: image.get("height").and_then(Value::as_u64),
                extra: image.as_object().cloned().unwrap_or_default(),
            },
        );
    }

    if parsed.is_empty() {
        None
    } else {
        Some(PinterestMedia {
            media_type: Some("image".to_string()),
            images: parsed,
            url: None,
            extra: Map::new(),
        })
    }
}

fn normalize_pinterest_datetime(value: String) -> String {
    DateTime::parse_from_rfc3339(&value)
        .or_else(|_| DateTime::parse_from_rfc2822(&value))
        .map(|datetime| datetime.to_rfc3339())
        .unwrap_or(value)
}

fn parse_aggregated_pin_data_from_html(html: &str) -> Option<AggregatedPinData> {
    let marker = "\"aggregatedPinData\":";
    let start = html.find(marker)? + marker.len();
    let object = json_object_prefix(&html[start..])?;
    let value = serde_json::from_str::<Value>(object).ok()?;
    Some(AggregatedPinData {
        entity_id: string_field(&value, "entityId")?,
        comment_count: value.get("commentCount").and_then(Value::as_u64),
    })
}

fn json_object_prefix(raw: &str) -> Option<&str> {
    // Pinterest embeds JSON inside larger JavaScript payloads. This scanner returns
    // the first complete object while respecting quoted braces and escapes.
    let raw = raw.trim_start();
    if !raw.starts_with('{') {
        return None;
    }

    let mut depth = 0_u32;
    let mut in_string = false;
    let mut escaped = false;

    for (offset, character) in raw.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }

        match character {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(&raw[..=offset]);
                }
            }
            _ => {}
        }
    }

    None
}

fn parse_public_pin_comments_response(
    value: &Value,
    max_comments: usize,
) -> Result<Vec<PublicPinComment>> {
    parse_public_pin_comments_response_with_parent(value, max_comments, None)
}

fn parse_public_pin_comment_replies_response(
    value: &Value,
    max_replies: usize,
    parent_comment_id: &str,
) -> Result<Vec<PublicPinComment>> {
    parse_public_pin_comments_response_with_parent(value, max_replies, Some(parent_comment_id))
}

fn parse_public_pin_comments_response_with_parent(
    value: &Value,
    max_comments: usize,
    parent_comment_id: Option<&str>,
) -> Result<Vec<PublicPinComment>> {
    let response = value
        .get("resource_response")
        .ok_or_else(|| anyhow!("Pinterest comments response has no resource_response"))?;
    if let Some(error) = response.get("error") {
        return Err(anyhow!(
            "Pinterest comments response error: {}",
            pinterest_error_message(error)
        ));
    }

    let data = response
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Pinterest comments response has no data array"))?;

    Ok(data
        .iter()
        .filter_map(|comment| parse_public_pin_comment(comment, parent_comment_id))
        .take(max_comments)
        .collect())
}

fn parse_public_pin_comment(
    value: &Value,
    parent_comment_id: Option<&str>,
) -> Option<PublicPinComment> {
    let text = string_field(value, "text")?;
    Some(PublicPinComment {
        id: string_field(value, "id"),
        text,
        created_at: string_field(value, "created_at"),
        parent_comment_id: parent_comment_id
            .map(str::to_string)
            .or_else(|| string_field(value, "parent_comment_id")),
        user_id: nested_string_field(value, &["user", "id"]),
        user_username: nested_string_field(value, &["user", "username"]),
        user_full_name: nested_string_field(value, &["user", "full_name"]),
        user_url: nested_string_field(value, &["user", "url"]),
        reply_count: value.get("comment_count").and_then(Value::as_u64),
    })
}

fn pinterest_error_message(error: &Value) -> String {
    string_field(error, "message_detail")
        .or_else(|| string_field(error, "message"))
        .unwrap_or_else(|| error.to_string())
}

fn configured_pinterest_cookie_header(raw: Option<&str>) -> Result<Option<HeaderValue>> {
    raw.map(|cookie| {
        HeaderValue::from_str(cookie.trim())
            .context("PINTEREST_COOKIE must be a valid HTTP Cookie header")
    })
    .transpose()
}

fn response_cookie_header(headers: &HeaderMap) -> Option<HeaderValue> {
    // Convert Set-Cookie headers from the initial HTML response into one Cookie
    // header for subsequent resource calls. Attributes such as Path and Expires
    // must not be copied into the request header.
    let cookie = headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|value| value.split(';').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("; ");

    if cookie.is_empty() {
        None
    } else {
        HeaderValue::from_str(&cookie).ok()
    }
}

fn merge_cookie_headers(
    configured_cookie_header: Option<&HeaderValue>,
    response_cookie_header: Option<&HeaderValue>,
) -> Option<HeaderValue> {
    let mut cookies = BTreeMap::new();
    append_cookie_header_parts(&mut cookies, configured_cookie_header);
    append_cookie_header_parts(&mut cookies, response_cookie_header);

    if cookies.is_empty() {
        return None;
    }

    let cookie = cookies
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; ");
    HeaderValue::from_str(&cookie).ok()
}

fn append_cookie_header_parts(
    cookies: &mut BTreeMap<String, String>,
    cookie_header: Option<&HeaderValue>,
) {
    let Some(cookie_header) = cookie_header else {
        return;
    };
    let Ok(cookie_header) = cookie_header.to_str() else {
        return;
    };

    for part in cookie_header.split(';') {
        let Some((name, value)) = part.trim().split_once('=') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        cookies.insert(name.to_string(), value.trim().to_string());
    }
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

fn collect_json_ld_pins(value: &Value, pins: &mut Vec<SavedPin>, seen: &mut BTreeSet<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_json_ld_pins(item, pins, seen);
            }
        }
        Value::Object(object) => {
            if let Some(parts) = object.get("hasPart").and_then(Value::as_array) {
                for part in parts {
                    collect_json_ld_pins(part, pins, seen);
                }
            }

            if let Some(saved_pin) = parse_json_ld_pin(value) {
                append_unique_pin(pins, seen, saved_pin);
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
            created_at: string_field(value, "datePublished").map(normalize_pinterest_datetime),
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

    fn fixture_json(raw: &str) -> Value {
        serde_json::from_str(raw).expect("fixture JSON")
    }

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
        let html = include_str!("../tests/fixtures/pinterest/public_profile_json_ld.html");

        let page = parse_public_profile_html(html).unwrap();

        assert_eq!(page.pins.len(), 1);
        assert_eq!(page.next_bookmark, None);
        assert_eq!(page.diagnostics.json_ld_scripts, 1);
        assert_eq!(page.diagnostics.json_ld_parse_errors, 0);
        assert_eq!(page.diagnostics.json_ld_pins, 1);
        assert!(!page.diagnostics.user_pins_resource_marker_found);
        assert_eq!(page.pins[0].pin.id, "123456789");
        assert_eq!(page.pins[0].pin.title.as_deref(), Some("Example pin title"));
        assert_eq!(
            page.pins[0].pin.best_image_url(),
            Some("https://i.pinimg.com/736x/example.jpg")
        );
        assert_eq!(
            page.pins[0]
                .pin
                .extra
                .get("public_author")
                .and_then(Value::as_str),
            Some("example-author")
        );
    }

    #[test]
    fn parses_public_profile_user_pins_resource() {
        let html = include_str!("../tests/fixtures/pinterest/user_pins_resource.html");

        let page = parse_public_profile_html(html).unwrap();

        assert_eq!(page.next_bookmark.as_deref(), Some("bookmark-1"));
        assert_eq!(page.pins.len(), 1);
        assert!(page.diagnostics.user_pins_resource_marker_found);
        assert!(page.diagnostics.user_pins_resource_json_found);
        assert_eq!(page.diagnostics.user_pins_resource_objects, 1);
        assert_eq!(page.diagnostics.user_pins_resource_data_arrays, 1);
        assert_eq!(page.diagnostics.user_pins_resource_pins, 2);
        assert_eq!(page.diagnostics.json_ld_scripts, 0);
        let saved = &page.pins[0];
        assert_eq!(saved.pin.id, "111");
        assert_eq!(
            saved.pin.title.as_deref(),
            Some("Nintendo ds | Tecnologia, Gadgets")
        );
        assert_eq!(
            saved.pin.created_at.as_deref(),
            Some("2026-06-15T02:59:57+00:00")
        );
        assert_eq!(saved.pin.board_id.as_deref(), Some("board-1"));
        assert_eq!(
            saved
                .pin
                .board_owner
                .as_ref()
                .and_then(|owner| owner.username.as_deref()),
            Some("vitalyzdanevich")
        );
        assert_eq!(
            saved.board.as_ref().and_then(|board| board.name.as_deref()),
            Some("tech")
        );
        assert_eq!(
            saved.pin.best_image_url(),
            Some("https://i.pinimg.com/736x/example.jpg")
        );
        assert_eq!(
            saved.pin.extra.get("public_author").and_then(Value::as_str),
            Some("Author Name")
        );
    }

    #[test]
    fn parses_public_profile_user_pins_page_response() {
        let response = fixture_json(include_str!(
            "../tests/fixtures/pinterest/user_pins_page_response.json"
        ));

        let page = parse_public_profile_pin_page_response(&response).unwrap();

        assert_eq!(page.next_bookmark.as_deref(), Some("bookmark-2"));
        assert_eq!(page.pins.len(), 1);
        assert_eq!(page.pins[0].pin.id, "222");
        assert_eq!(
            page.pins[0].pin.best_image_url(),
            Some("https://i.pinimg.com/474x/page2.jpg")
        );
        assert_eq!(page.diagnostics, PublicProfileParseDiagnostics::default());
    }

    #[test]
    fn records_public_profile_parser_stage_diagnostics() {
        let html = r#"
            <script type="application/ld+json">{invalid json</script>
            <script>window.__PWS_DATA__ = {"resources": {"UserPinsResource": {"bad": []}}};</script>
        "#;

        let page = parse_public_profile_html(html).unwrap();

        assert!(page.pins.is_empty());
        assert!(page.diagnostics.user_pins_resource_marker_found);
        assert!(page.diagnostics.user_pins_resource_json_found);
        assert_eq!(page.diagnostics.user_pins_resource_objects, 1);
        assert_eq!(page.diagnostics.user_pins_resource_data_arrays, 0);
        assert_eq!(page.diagnostics.user_pins_resource_pins, 0);
        assert_eq!(page.diagnostics.json_ld_scripts, 1);
        assert_eq!(page.diagnostics.json_ld_parse_errors, 1);
        assert_eq!(page.diagnostics.json_ld_pins, 0);

        let rendered = profile_parse_diagnostics(&page.diagnostics);
        assert!(rendered.contains("UserPinsResource marker_found=true"));
        assert!(rendered.contains("data_arrays=0"));
        assert!(rendered.contains("JSON-LD scripts=1"));
        assert!(rendered.contains("parse_errors=1"));
    }

    #[test]
    fn builds_api_and_public_profile_urls() {
        assert_eq!(
            endpoint("https://api.pinterest.com/v5/", "pins")
                .unwrap()
                .as_str(),
            "https://api.pinterest.com/v5/pins"
        );
        assert_eq!(
            endpoint("https://api.pinterest.com/v5", "/boards")
                .unwrap()
                .as_str(),
            "https://api.pinterest.com/v5/boards"
        );
        assert_eq!(
            public_profile_url("vitalyzdanevich").unwrap().as_str(),
            "https://www.pinterest.com/vitalyzdanevich/pins/"
        );
        assert_eq!(
            public_profile_url("vitalyzdanevich/pins").unwrap().as_str(),
            "https://www.pinterest.com/vitalyzdanevich/pins/"
        );
        assert!(public_profile_url("  ").is_err());
        assert!(public_profile_url("https://[").is_err());
    }

    #[test]
    fn extracts_public_profile_username_from_url() {
        let url = Url::parse("https://www.pinterest.com/vitalyzdanevich/pins/").unwrap();
        assert_eq!(
            public_profile_username(&url),
            Some("vitalyzdanevich".to_string())
        );

        let root = Url::parse("https://www.pinterest.com/").unwrap();
        assert_eq!(public_profile_username(&root), None);
    }

    #[test]
    fn parses_public_profile_response_errors() {
        let missing_response = serde_json::json!({});
        assert!(
            parse_public_profile_pin_page_response(&missing_response)
                .expect_err("missing resource_response")
                .to_string()
                .contains("no resource_response")
        );

        let error_response = serde_json::json!({
            "resource_response": {
                "error": {"message_detail": "private profile"}
            }
        });
        assert!(
            parse_public_profile_pin_page_response(&error_response)
                .expect_err("resource error")
                .to_string()
                .contains("private profile")
        );

        let missing_data = serde_json::json!({"resource_response": {"data": {}}});
        assert!(
            parse_public_profile_pin_page_response(&missing_data)
                .expect_err("missing data array")
                .to_string()
                .contains("no data array")
        );
    }

    #[test]
    fn parses_public_profile_resource_pin_fallback_fields() {
        let value = fixture_json(include_str!(
            "../tests/fixtures/pinterest/public_profile_resource_pin_fallback.json"
        ));

        let saved = parse_public_profile_resource_pin(&value).expect("public pin");

        assert_eq!(saved.pin.id, "333");
        assert_eq!(saved.pin.title.as_deref(), Some("Fallback title"));
        assert_eq!(
            saved.pin.link.as_deref(),
            Some("https://example.com/tracked")
        );
        assert_eq!(saved.pin.alt_text.as_deref(), Some("Generated alt"));
        assert_eq!(saved.pin.created_at.as_deref(), Some("not a date"));
        assert_eq!(saved.pin.board_id.as_deref(), Some("board-2"));
        assert_eq!(saved.pin.parent_pin_id.as_deref(), Some("parent-1"));
        assert!(saved.pin.media.is_none());
        assert_eq!(
            saved.pin.extra.get("public_author").and_then(Value::as_str),
            Some("native_author")
        );
        assert_eq!(
            saved
                .pin
                .extra
                .get("public_source_domain")
                .and_then(Value::as_str),
            Some("example.com")
        );
    }

    #[test]
    fn extracts_balanced_json_objects_from_javascript() {
        let raw = r#"{"text":"brace } inside string","nested":{"escaped":"quote \" ok"}} trailing"#;

        assert_eq!(
            json_object_prefix(raw),
            Some(r#"{"text":"brace } inside string","nested":{"escaped":"quote \" ok"}}"#)
        );
        assert_eq!(json_object_prefix("[]"), None);
        assert_eq!(json_object_prefix(r#"{"unfinished": true"#), None);
    }

    #[test]
    fn extracts_cookie_header_from_set_cookie_headers() {
        let mut headers = HeaderMap::new();
        headers.append(
            SET_COOKIE,
            HeaderValue::from_static("csrftoken=abc; Path=/; Secure"),
        );
        headers.append(
            SET_COOKIE,
            HeaderValue::from_static("session=def; HttpOnly"),
        );

        let cookie = response_cookie_header(&headers).expect("cookie header");

        assert_eq!(cookie.to_str().unwrap(), "csrftoken=abc; session=def");
    }

    #[test]
    fn validates_configured_pinterest_cookie_header() {
        let cookie = configured_pinterest_cookie_header(Some("session=abc; csrftoken=def"))
            .unwrap()
            .expect("configured cookie");

        assert_eq!(cookie.to_str().unwrap(), "session=abc; csrftoken=def");
        assert!(configured_pinterest_cookie_header(Some("bad\ncookie")).is_err());
    }

    #[test]
    fn merges_configured_and_response_cookie_headers() {
        let configured = HeaderValue::from_static("_pinterest_sess=logged-in; csrftoken=old-token");
        let response = HeaderValue::from_static("csrftoken=new-token; unauth_id=guest");

        let cookie =
            merge_cookie_headers(Some(&configured), Some(&response)).expect("merged cookie header");

        assert_eq!(
            cookie.to_str().unwrap(),
            "_pinterest_sess=logged-in; csrftoken=new-token; unauth_id=guest"
        );
    }

    #[test]
    fn extracts_json_ld_image_urls_and_authors() {
        let image = serde_json::json!([
            {"contentUrl": "https://example.com/content.jpg"},
            "https://example.com/fallback.jpg"
        ]);
        let author = serde_json::json!([
            {"alternateName": "@author"},
            {"name": "Author Name"}
        ]);

        assert_eq!(
            image_url_from_value(Some(&image)).as_deref(),
            Some("https://example.com/content.jpg")
        );
        assert_eq!(author_name(Some(&author)).as_deref(), Some("@author"));
        assert_eq!(image_url_from_value(Some(&Value::Null)), None);
        assert_eq!(author_name(Some(&Value::Bool(true))), None);
    }

    #[test]
    fn public_pin_comments_are_limited_and_errors_are_reported() {
        let response = serde_json::json!({
            "resource_response": {
                "status": "success",
                "data": [
                    {"id": "comment-1", "text": "One"},
                    {"id": "comment-2", "text": "Two"}
                ]
            }
        });

        let comments = parse_public_pin_comments_response(&response, 1).unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].id.as_deref(), Some("comment-1"));

        let error_response = serde_json::json!({
            "resource_response": {
                "error": {"message": "comments disabled"}
            }
        });
        assert!(
            parse_public_pin_comments_response(&error_response, 10)
                .expect_err("comments error")
                .to_string()
                .contains("comments disabled")
        );

        let missing_data = serde_json::json!({"resource_response": {}});
        assert!(
            parse_public_pin_comments_response(&missing_data, 10)
                .expect_err("missing comments data")
                .to_string()
                .contains("no data array")
        );
    }

    #[test]
    fn attaches_public_comments_to_extra_only_when_present() {
        let mut extra = Map::new();
        PublicPinComments {
            total_count: Some(0),
            comments: Vec::new(),
        }
        .attach_to_extra(&mut extra);

        assert_eq!(
            extra.get("public_comment_count").and_then(Value::as_u64),
            Some(0)
        );
        assert!(extra.get("public_comments").is_none());
    }

    #[test]
    fn parses_aggregated_pin_data_from_public_pin_html() {
        let html = include_str!("../tests/fixtures/pinterest/aggregated_pin_data.html");

        let data = parse_aggregated_pin_data_from_html(html).unwrap();

        assert_eq!(data.entity_id, "5302154233464808675");
        assert_eq!(data.comment_count, Some(5));
    }

    #[test]
    fn parses_public_pin_comments_response() {
        let response = fixture_json(include_str!(
            "../tests/fixtures/pinterest/public_pin_comments_response.json"
        ));

        let comments = parse_public_pin_comments_response(&response, 10).unwrap();

        assert_eq!(
            comments,
            vec![PublicPinComment {
                id: Some("comment-1".to_string()),
                text: "Hello <world>".to_string(),
                created_at: Some("Mon, 15 Jun 2026 10:00:00 +0000".to_string()),
                parent_comment_id: None,
                user_id: Some("user-1".to_string()),
                user_username: Some("commenter".to_string()),
                user_full_name: Some("Commenter Name".to_string()),
                user_url: Some("/commenter/".to_string()),
                reply_count: Some(2),
            }]
        );
    }

    #[test]
    fn parses_public_pin_comment_replies_response() {
        let response = serde_json::json!({
            "resource_response": {
                "status": "success",
                "data": [
                    {
                        "id": "reply-1",
                        "text": "Nested reply",
                        "created_at": "Tue, 16 Jun 2026 12:00:00 +0000",
                        "user": {"id": "user-2"}
                    }
                ]
            }
        });

        let comments =
            parse_public_pin_comment_replies_response(&response, 10, "comment-1").unwrap();

        assert_eq!(
            comments,
            vec![PublicPinComment {
                id: Some("reply-1".to_string()),
                text: "Nested reply".to_string(),
                created_at: Some("Tue, 16 Jun 2026 12:00:00 +0000".to_string()),
                parent_comment_id: Some("comment-1".to_string()),
                user_id: Some("user-2".to_string()),
                user_username: None,
                user_full_name: None,
                user_url: None,
                reply_count: None,
            }]
        );
    }
}
