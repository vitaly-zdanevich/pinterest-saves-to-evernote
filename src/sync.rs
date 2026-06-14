use anyhow::Result;
use chrono::Utc;
use tracing::{info, warn};

use crate::config::Settings;
use crate::evernote_client::EvernoteClient;
use crate::image::ImageDownloader;
use crate::note;
use crate::pinterest::{
    PinterestClient, SavedPin, public_profile_saved_pins, resolve_access_token,
};
use crate::state::State;

pub async fn run(settings: Settings) -> Result<()> {
    let mut state = State::load(&settings.state_path)?;
    let mut saved_pins = if settings.public_profile_to_parse_without_api.is_some() {
        public_profile_saved_pins(&settings).await?
    } else {
        let access_token = resolve_access_token(&settings).await?;
        let pinterest = PinterestClient::new(&settings, access_token)?;
        pinterest.saved_pins(&settings).await?
    };

    saved_pins.sort_by(|left, right| {
        left.pin
            .created_at_utc()
            .cmp(&right.pin.created_at_utc())
            .then_with(|| left.pin.id.cmp(&right.pin.id))
    });

    if !state.is_initialized() && !settings.backfill_existing {
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

    let evernote = if settings.dry_run {
        None
    } else {
        Some(EvernoteClient::new(
            settings.evernote_auth_token.clone(),
            Some(settings.evernote_user_store_url.clone()),
            settings.evernote_note_store_url.clone(),
            settings.evernote_notebook_guid.clone(),
            settings.evernote_notebook_name.clone(),
            settings.evernote_tags.clone(),
        )?)
    };
    let image_downloader = if settings.attach_images {
        Some(ImageDownloader::new(settings.max_image_bytes)?)
    } else {
        None
    };

    for saved in new_pins {
        export_pin(
            &settings,
            &mut state,
            evernote.as_ref(),
            image_downloader.as_ref(),
            saved,
        )
        .await?;
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
    evernote: Option<&EvernoteClient>,
    image_downloader: Option<&ImageDownloader>,
    saved: SavedPin,
) -> Result<()> {
    let title = note::title(&saved);
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
            image_attached = image.is_some(),
            "dry-run: would create Evernote note"
        );
        return Ok(());
    }

    if let Some(evernote) = evernote {
        let guid =
            evernote.create_pin_note(title.clone(), content, image.as_ref(), pin_url.clone())?;
        info!(
            pin_id = saved.pin.id,
            evernote_guid = guid,
            title = title,
            pin_url = pin_url,
            image_attached = image.is_some(),
            "created Evernote note"
        );
    }

    state.mark_processed(saved.pin.id);
    state.save(&settings.state_path)?;
    Ok(())
}
