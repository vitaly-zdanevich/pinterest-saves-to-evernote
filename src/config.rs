use std::env;
use std::path::PathBuf;

use anyhow::{Result, anyhow};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PinterestFetchMode {
    Boards,
    Account,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub pinterest_access_token: Option<String>,
    pub pinterest_client_id: Option<String>,
    pub pinterest_client_secret: Option<String>,
    pub pinterest_refresh_token: Option<String>,
    pub public_profile_to_parse_without_api: Option<String>,
    pub pinterest_token_scope: String,
    pub pinterest_api_base_url: String,
    pub pinterest_board_ids: Vec<String>,
    pub pinterest_fetch_mode: PinterestFetchMode,
    pub pinterest_include_sections: bool,
    pub evernote_auth_token: String,
    pub evernote_note_store_url: Option<String>,
    pub evernote_user_store_url: String,
    pub evernote_notebook_guid: Option<String>,
    pub evernote_notebook_name: Option<String>,
    pub evernote_tags: Vec<String>,
    pub state_path: PathBuf,
    pub dry_run: bool,
    pub backfill_existing: bool,
    pub max_pins_per_run: usize,
    pub page_size: usize,
    pub attach_images: bool,
    pub max_image_bytes: u64,
    pub scrape_pin_comments: bool,
    pub max_pin_comments: usize,
}

impl Settings {
    pub fn from_env() -> Result<Self> {
        let pinterest_access_token = optional_env("PINTEREST_ACCESS_TOKEN");
        let pinterest_client_id = optional_env("PINTEREST_CLIENT_ID");
        let pinterest_client_secret = optional_env("PINTEREST_CLIENT_SECRET");
        let pinterest_refresh_token = optional_env("PINTEREST_REFRESH_TOKEN");
        let public_profile_to_parse_without_api =
            optional_env("PUBLIC_PROFILE_TO_PARSE_WITHOUT_API");

        if public_profile_to_parse_without_api.is_none()
            && pinterest_access_token.is_none()
            && pinterest_refresh_token.is_none()
        {
            return Err(anyhow!(
                "PINTEREST_ACCESS_TOKEN, PINTEREST_REFRESH_TOKEN, or PUBLIC_PROFILE_TO_PARSE_WITHOUT_API is required"
            ));
        }
        if pinterest_refresh_token.is_some()
            && (pinterest_client_id.is_none() || pinterest_client_secret.is_none())
        {
            return Err(anyhow!(
                "PINTEREST_CLIENT_ID and PINTEREST_CLIENT_SECRET are required when PINTEREST_REFRESH_TOKEN is set"
            ));
        }

        let evernote_auth_token = optional_env("EVERNOTE_AUTH_TOKEN")
            .or_else(|| optional_env("EVERNOTE_TOKEN"))
            .ok_or_else(|| anyhow!("EVERNOTE_AUTH_TOKEN or EVERNOTE_TOKEN is required"))?;
        let evernote_notebook_guid = optional_env("EVERNOTE_NOTEBOOK_GUID");
        let evernote_notebook_name = optional_env("EVERNOTE_NOTEBOOK_NAME");
        if evernote_notebook_guid.is_some() && evernote_notebook_name.is_some() {
            return Err(anyhow!(
                "set only one of EVERNOTE_NOTEBOOK_GUID or EVERNOTE_NOTEBOOK_NAME"
            ));
        }

        let max_pins_per_run = parse_usize_env("MAX_PINS_PER_RUN", 25)?;
        if max_pins_per_run == 0 {
            return Err(anyhow!("MAX_PINS_PER_RUN must be greater than 0"));
        }

        let page_size = parse_usize_env("PINTEREST_PAGE_SIZE", 100)?;
        if page_size == 0 || page_size > 250 {
            return Err(anyhow!("PINTEREST_PAGE_SIZE must be between 1 and 250"));
        }

        let max_image_bytes = parse_u64_env("MAX_IMAGE_BYTES", 25 * 1024 * 1024)?;
        if max_image_bytes == 0 {
            return Err(anyhow!("MAX_IMAGE_BYTES must be greater than 0"));
        }

        let max_pin_comments = parse_usize_env("MAX_PIN_COMMENTS", 25)?;
        if max_pin_comments == 0 {
            return Err(anyhow!("MAX_PIN_COMMENTS must be greater than 0"));
        }

        Ok(Self {
            pinterest_access_token,
            pinterest_client_id,
            pinterest_client_secret,
            pinterest_refresh_token,
            public_profile_to_parse_without_api,
            pinterest_token_scope: env_or("PINTEREST_TOKEN_SCOPE", "boards:read,pins:read"),
            pinterest_api_base_url: env_or(
                "PINTEREST_API_BASE_URL",
                "https://api.pinterest.com/v5",
            ),
            pinterest_board_ids: parse_list(&env::var("PINTEREST_BOARD_IDS").unwrap_or_default()),
            pinterest_fetch_mode: parse_fetch_mode(&env_or("PINTEREST_FETCH_MODE", "boards"))?,
            pinterest_include_sections: parse_bool_env("PINTEREST_INCLUDE_SECTIONS", true)?,
            evernote_auth_token,
            evernote_note_store_url: optional_env("EVERNOTE_NOTE_STORE_URL"),
            evernote_user_store_url: env_or(
                "EVERNOTE_USER_STORE_URL",
                "https://www.evernote.com/edam/user",
            ),
            evernote_notebook_guid,
            evernote_notebook_name,
            evernote_tags: parse_tags(&env_or("EVERNOTE_TAGS", "pinterest")),
            state_path: PathBuf::from(env_or("STATE_PATH", "state/state.json")),
            dry_run: parse_bool_env("DRY_RUN", false)?,
            backfill_existing: parse_bool_env("BACKFILL_EXISTING", false)?,
            max_pins_per_run,
            page_size,
            attach_images: parse_bool_env("ATTACH_IMAGES", true)?,
            max_image_bytes,
            scrape_pin_comments: parse_bool_env("SCRAPE_PIN_COMMENTS", true)?,
            max_pin_comments,
        })
    }

    pub fn can_refresh_pinterest_token(&self) -> bool {
        self.pinterest_refresh_token.is_some()
            && self.pinterest_client_id.is_some()
            && self.pinterest_client_secret.is_some()
    }
}

pub fn parse_list(raw: &str) -> Vec<String> {
    raw.replace(',', " ")
        .split_whitespace()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

pub fn parse_bool(raw: &str) -> Result<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "y" | "on" => Ok(true),
        "0" | "false" | "no" | "n" | "off" => Ok(false),
        _ => Err(anyhow!("invalid boolean value: {raw}")),
    }
}

fn parse_fetch_mode(raw: &str) -> Result<PinterestFetchMode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "boards" | "board" => Ok(PinterestFetchMode::Boards),
        "account" | "pins" => Ok(PinterestFetchMode::Account),
        _ => Err(anyhow!("PINTEREST_FETCH_MODE must be boards or account")),
    }
}

fn parse_tags(raw: &str) -> Vec<String> {
    let tags = parse_list(raw);
    if tags.is_empty() {
        vec!["pinterest".to_string()]
    } else {
        tags
    }
}

fn parse_bool_env(name: &str, default: bool) -> Result<bool> {
    match optional_env(name) {
        Some(value) => parse_bool(&value).map_err(|error| anyhow!("{name}: {error}")),
        None => Ok(default),
    }
}

fn parse_usize_env(name: &str, default: usize) -> Result<usize> {
    match optional_env(name) {
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| anyhow!("{name} must be an unsigned integer")),
        None => Ok(default),
    }
}

fn parse_u64_env(name: &str, default: u64) -> Result<u64> {
    match optional_env(name) {
        Some(value) => value
            .parse::<u64>()
            .map_err(|_| anyhow!("{name} must be an unsigned integer")),
        None => Ok(default),
    }
}

fn optional_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn env_or(name: &str, default: &str) -> String {
    optional_env(name).unwrap_or_else(|| default.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_comma_and_space_lists() {
        assert_eq!(
            parse_list("a,b c,, d"),
            vec![
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string()
            ]
        );
    }

    #[test]
    fn parses_boolean_values() {
        assert!(parse_bool("true").unwrap());
        assert!(parse_bool("YES").unwrap());
        assert!(!parse_bool("0").unwrap());
        assert!(parse_bool("maybe").is_err());
    }
}
