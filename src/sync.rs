use std::collections::BTreeSet;

use anyhow::{Context, Result};
use chrono::Utc;
use tokio::task;
use tracing::{info, warn};

use crate::config::Settings;
use crate::evernote_client::EvernoteClient;
use crate::image::ImageDownloader;
use crate::note;
use crate::pinterest::{
    PinterestClient, SavedPin, public_profile_saved_pins, resolve_access_token,
    scrape_public_pin_comments,
};
use crate::state::State;

pub async fn run(settings: Settings) -> Result<()> {
    let mut state = State::load(&settings.state_path)?;
    let using_public_profile = settings.public_profile_to_parse_without_api.is_some();
    let mut saved_pins = if using_public_profile {
        public_profile_saved_pins(&settings).await?
    } else {
        let access_token = resolve_access_token(&settings).await?;
        let pinterest = PinterestClient::new(&settings, access_token)?;
        pinterest.saved_pins(&settings).await?
    };

    if using_public_profile && state.is_initialized() && !settings.backfill_existing {
        // The public profile feed is chronological newest-first. Once we reach a
        // pin that is already in state, older entries on later pages were already
        // seen too, so stop before exporting duplicates.
        if let Some(first_processed_index) = saved_pins
            .iter()
            .position(|saved| state.contains(&saved.pin.id))
        {
            let skipped_existing_and_older = saved_pins.len().saturating_sub(first_processed_index);
            saved_pins.truncate(first_processed_index);
            info!(
                new_or_unseen_before_processed = saved_pins.len(),
                skipped_existing_and_older,
                "stopped public Pinterest profile scan at first already processed pin"
            );
        }
    }

    saved_pins.sort_by(|left, right| {
        // Export oldest-to-newest so Evernote note order follows the original save
        // order when a run imports several new pins at once.
        left.pin
            .created_at_utc()
            .cmp(&right.pin.created_at_utc())
            .then_with(|| left.pin.id.cmp(&right.pin.id))
    });

    if !state.is_initialized() && !settings.backfill_existing {
        // First run establishes a baseline only. Without this guard, enabling the
        // workflow would import every old saved pin in the account/profile.
        info!(
            pins = saved_pins.len(),
            "first run baseline: marking existing Pinterest pins as already processed"
        );
        if !settings.dry_run {
            for saved in saved_pins {
                state.mark_processed(saved.pin.id);
            }
            state.initialized_at = Some(Utc::now());
            state.last_successful_sync_at = Some(Utc::now());
            state.save(&settings.state_path)?;
        }
        return Ok(());
    }

    let mut new_pins = saved_pins
        .into_iter()
        .filter(|saved| !state.contains(&saved.pin.id))
        .collect::<Vec<_>>();

    if new_pins.len() > settings.max_pins_per_run {
        warn!(
            total_new_pins = new_pins.len(),
            limit = settings.max_pins_per_run,
            "limiting Pinterest pins exported in this run"
        );
        new_pins.truncate(settings.max_pins_per_run);
    }

    info!(new_pins = new_pins.len(), "found new Pinterest pins");
    if new_pins.is_empty() {
        if !settings.dry_run {
            state.last_successful_sync_at = Some(Utc::now());
            state.save(&settings.state_path)?;
        }
        return Ok(());
    }

    let image_downloader = if settings.attach_images {
        Some(ImageDownloader::new(settings.max_image_bytes)?)
    } else {
        None
    };

    for saved in new_pins {
        export_pin(&settings, &mut state, image_downloader.as_ref(), saved).await?;
    }

    if !settings.dry_run {
        state.last_successful_sync_at = Some(Utc::now());
        state.save(&settings.state_path)?;
    }

    Ok(())
}

async fn export_pin(
    settings: &Settings,
    state: &mut State,
    image_downloader: Option<&ImageDownloader>,
    mut saved: SavedPin,
) -> Result<()> {
    if settings.scrape_pin_comments {
        // Comment scraping is best-effort because it depends on Pinterest's public
        // web endpoints. A comments failure must not block exporting the pin itself.
        match scrape_public_pin_comments(
            &saved.pin.id,
            settings.max_pin_comments,
            settings.pinterest_cookie.as_deref(),
        )
        .await
        {
            Ok(comments) => comments.attach_to_extra(&mut saved.pin.extra),
            Err(error) => warn!(
                pin_id = saved.pin.id,
                error = %error,
                "failed to scrape public Pinterest comments; continuing without comments"
            ),
        }
    }

    let title = note::title(&saved);
    let tags = merged_note_tags(&settings.evernote_tags, note::pin_hashtags(&saved));
    let image_url = saved.pin.best_image_url().map(ToOwned::to_owned);
    let image =
        if let (Some(downloader), Some(image_url)) = (image_downloader, image_url.as_deref()) {
            Some(downloader.download(image_url, &saved.pin.id).await?)
        } else {
            None
        };
    let content = note::enml(&saved, image.as_ref());
    let pin_url = saved.pin.pin_url();

    if settings.dry_run {
        info!(
            pin_id = saved.pin.id,
            title = title,
            pin_url = pin_url,
            tags = ?tags,
            image_attached = image.is_some(),
            "dry-run: would create Evernote note"
        );
        return Ok(());
    }

    let image_attached = image.is_some();
    let guid = create_evernote_note_blocking(
        settings,
        title.clone(),
        content,
        image,
        pin_url.clone(),
        tags.clone(),
    )
    .await?;
    info!(
        pin_id = saved.pin.id,
        evernote_guid = guid,
        title = title,
        pin_url = pin_url,
        tags = ?tags,
        image_attached = image_attached,
        "created Evernote note"
    );

    state.mark_processed(saved.pin.id);
    state.save(&settings.state_path)?;
    Ok(())
}

async fn create_evernote_note_blocking(
    settings: &Settings,
    title: String,
    content: String,
    image: Option<crate::image::DownloadedImage>,
    pin_url: String,
    tags: Vec<String>,
) -> Result<String> {
    // The Evernote Thrift client and reqwest transport are synchronous. Running
    // them in a blocking task prevents Tokio from panicking when the runtime is
    // dropped from inside an async context.
    let token = settings.evernote_auth_token.clone();
    let user_store_url = settings.evernote_user_store_url.clone();
    let note_store_url = settings.evernote_note_store_url.clone();
    let notebook_guid = settings.evernote_notebook_guid.clone();
    let notebook_name = settings.evernote_notebook_name.clone();
    let source_url = pin_url.clone();

    task::spawn_blocking(move || {
        let evernote = EvernoteClient::new(
            token,
            Some(user_store_url),
            note_store_url,
            notebook_guid,
            notebook_name,
            tags,
        )?;
        evernote.create_pin_note(title, content, image.as_ref(), source_url)
    })
    .await
    .context("Evernote worker task panicked or was cancelled")?
}

fn merged_note_tags(base_tags: &[String], title_tags: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut tags = Vec::new();

    for tag in base_tags.iter().chain(title_tags.iter()) {
        let tag = tag.trim();
        if tag.is_empty() {
            continue;
        }
        if seen.insert(tag.to_lowercase()) {
            tags.push(tag.to_string());
        }
    }

    tags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_configured_and_pin_tags_without_duplicates() {
        let tags = merged_note_tags(
            &["pinterest".to_string(), "nostalgia".to_string()],
            vec![
                "olderbrothercore".to_string(),
                "nostalgia".to_string(),
                "OlderBrotherCore".to_string(),
            ],
        );

        assert_eq!(
            tags,
            vec![
                "pinterest".to_string(),
                "nostalgia".to_string(),
                "olderbrothercore".to_string(),
            ]
        );
    }
}
