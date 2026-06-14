use html_escape::{encode_double_quoted_attribute, encode_safe};

use crate::image::DownloadedImage;
use crate::pinterest::SavedPin;

pub fn title(saved: &SavedPin) -> String {
    let raw = saved
        .pin
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Pinterest pin");
    truncate_title(raw)
}

pub fn enml(saved: &SavedPin, image: Option<&DownloadedImage>) -> String {
    let title = field("Title", saved.pin.title.as_deref());
    let description = multiline_field("Description", saved.pin.description.as_deref());
    let alt_text = multiline_field("Alt text", saved.pin.alt_text.as_deref());
    let created_at = field("Created at", saved.pin.created_at.as_deref());
    let board = saved
        .board
        .as_ref()
        .and_then(|board| board.name.as_deref().or(Some(board.id.as_str())));
    let section = saved
        .section
        .as_ref()
        .and_then(|section| section.name.as_deref().or(Some(section.id.as_str())));
    let board = field("Board", board);
    let section = field("Section", section);
    let owner = field(
        "Board owner",
        saved
            .pin
            .board_owner
            .as_ref()
            .and_then(|owner| owner.username.as_deref()),
    );
    let creative_type = field("Creative type", saved.pin.creative_type.as_deref());
    let parent_pin = field("Parent pin ID", saved.pin.parent_pin_id.as_deref());
    let image_url = saved.pin.best_image_url();
    let image_url_row = link_field("Image URL", image_url);
    let source_link = link_field("Source link", saved.pin.link.as_deref());
    let pin_url = saved.pin.pin_url();
    let pin_link = link_field("Pinterest pin", Some(&pin_url));
    let image_markup = image
        .map(|image| {
            let mime_type = encode_double_quoted_attribute(&image.mime_type);
            let hash = encode_double_quoted_attribute(&image.hash_hex);
            format!(
                "<div><en-media type=\"{mime_type}\" hash=\"{hash}\" /></div>\n<div><br/></div>"
            )
        })
        .unwrap_or_default();

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE en-note SYSTEM "http://xml.evernote.com/pub/enml2.dtd">
<en-note>
{image_markup}
<div><b>Pin ID:</b> {pin_id}</div>
{title}
{description}
{alt_text}
{created_at}
{board}
{section}
{owner}
{creative_type}
{parent_pin}
{pin_link}
{source_link}
{image_url_row}
</en-note>"#,
        pin_id = encode_safe(&saved.pin.id)
    )
}

fn field(label: &str, value: Option<&str>) -> String {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            format!(
                "<div><b>{}:</b> {}</div>",
                encode_safe(label),
                encode_safe(value)
            )
        })
        .unwrap_or_default()
}

fn multiline_field(label: &str, value: Option<&str>) -> String {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            let body = value
                .lines()
                .map(|line| encode_safe(line.trim()).to_string())
                .collect::<Vec<_>>()
                .join("<br/>");
            format!("<div><b>{}:</b> {body}</div>", encode_safe(label))
        })
        .unwrap_or_default()
}

fn link_field(label: &str, url: Option<&str>) -> String {
    url.map(str::trim)
        .filter(|url| !url.is_empty())
        .map(|url| {
            let href = encode_double_quoted_attribute(url);
            let text = encode_safe(url);
            format!(
                "<div><b>{}:</b> <a href=\"{href}\">{text}</a></div>",
                encode_safe(label)
            )
        })
        .unwrap_or_default()
}

fn truncate_title(raw: &str) -> String {
    const MAX_TITLE_CHARS: usize = 250;
    let mut title = raw.trim().replace(['\n', '\r'], " ");
    if title.chars().count() <= MAX_TITLE_CHARS {
        return title;
    }

    title = title.chars().take(MAX_TITLE_CHARS - 3).collect();
    title.push_str("...");
    title
}

#[cfg(test)]
mod tests {
    use serde_json::Map;

    use super::*;
    use crate::pinterest::{PinterestBoard, PinterestPin};

    #[test]
    fn renders_enml_with_escaped_values() {
        let saved = SavedPin {
            pin: PinterestPin {
                id: "123".to_string(),
                title: Some("A < B".to_string()),
                description: Some("Line 1\nLine & 2".to_string()),
                link: Some("https://example.com/?a=1&b=2".to_string()),
                created_at: Some("2026-01-02T03:04:05Z".to_string()),
                board_id: Some("board-1".to_string()),
                board_section_id: None,
                board_owner: None,
                parent_pin_id: None,
                alt_text: Some("Alt > text".to_string()),
                creative_type: Some("REGULAR".to_string()),
                media: None,
                extra: Map::new(),
            },
            board: Some(PinterestBoard {
                id: "board-1".to_string(),
                name: Some("Ideas".to_string()),
                extra: Map::new(),
            }),
            section: None,
        };

        let enml = enml(&saved, None);

        assert!(enml.contains("A &lt; B"));
        assert!(enml.contains("Line &amp; 2"));
        assert!(enml.contains("https://example.com/?a=1&amp;b=2"));
        assert!(enml.contains("Alt &gt; text"));
        assert!(enml.contains("Ideas"));
    }
}
