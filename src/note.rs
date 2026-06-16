use html_escape::{encode_double_quoted_attribute, encode_safe};
use serde_json::{Map, Value};

use crate::image::DownloadedImage;
use crate::pinterest::{PinterestBoard, SavedPin};

const PROJECT_URL: &str = "https://github.com/vitaly-zdanevich/pinterest-saves-to-evernote";

/// Build the Evernote note title from Pinterest metadata.
///
/// Hashtags are removed from the title because they are exported as Evernote tags.
pub fn title(saved: &SavedPin) -> String {
    let raw = saved.pin.title.as_deref();
    let title = raw
        .and_then(clean_title_without_hashtags)
        .unwrap_or_else(|| "Pinterest pin".to_string());
    truncate_title(&title)
}

pub fn title_hashtags(saved: &SavedPin) -> Vec<String> {
    saved
        .pin
        .title
        .as_deref()
        .map(hashtags_from_text)
        .unwrap_or_default()
}

pub fn enml(saved: &SavedPin, image: Option<&DownloadedImage>) -> String {
    // Evernote stores note bodies as ENML, an XHTML subset. Every value that comes
    // from Pinterest must be escaped before being inserted into the document.
    let description = multiline_field("Description", saved.pin.description.as_deref());
    let alt_text = multiline_field("Alt text", saved.pin.alt_text.as_deref());
    let section = saved
        .section
        .as_ref()
        .and_then(|section| section.name.as_deref().or(Some(section.id.as_str())));
    let board = board_field(saved.board.as_ref());
    let section = field("Section", section);
    let public_author = public_author_field(&saved.pin.extra);
    let public_comments = comments_section(&saved.pin.extra);
    let parent_pin = field("Parent pin ID", saved.pin.parent_pin_id.as_deref());
    let image_url = saved.pin.best_image_url();
    let image_url_row = link_field("Image URL", image_url);
    let source_link = link_field("Source link", saved.pin.link.as_deref());
    let imported_by = imported_by_field();
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
{description}
{alt_text}
{board}
{section}
{public_author}
{public_comments}
{parent_pin}
{source_link}
{image_url_row}
{imported_by}
</en-note>"#
    )
}

fn extra_string<'a>(extra: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    extra.get(key).and_then(Value::as_str)
}

fn board_field(board: Option<&PinterestBoard>) -> String {
    let Some(board) = board else {
        return String::new();
    };
    let name = board.name.as_deref().unwrap_or(board.id.as_str());

    match board_url(board) {
        Some(url) => link_text_field("Board", &url, name),
        None => field("Board", Some(name)),
    }
}

fn board_url(board: &PinterestBoard) -> Option<String> {
    extra_string(&board.extra, "url").and_then(pinterest_url)
}

fn pinterest_url(raw: &str) -> Option<String> {
    let url = raw.trim();
    if url.starts_with("https://www.pinterest.com/") {
        Some(url.to_string())
    } else if url.starts_with('/') {
        Some(format!("https://www.pinterest.com{url}"))
    } else {
        None
    }
}

fn public_author_field(extra: &Map<String, Value>) -> String {
    let author = extra_string(extra, "public_author");
    let author_username = extra_string(extra, "public_author_username");
    match (author, author_username) {
        (Some(author), Some(username)) => match pinterest_profile_url_for_username(username) {
            Some(url) => link_text_field("Pinterest author", &url, author),
            None => field("Pinterest author", Some(author)),
        },
        (None, Some(username)) => match pinterest_profile_url_for_username(username) {
            Some(url) => link_text_field("Pinterest author", &url, username),
            None => field("Pinterest author", Some(username)),
        },
        _ => field("Pinterest author", author),
    }
}

fn pinterest_profile_url_for_username(username: &str) -> Option<String> {
    let username = username.trim().trim_start_matches('@').trim_matches('/');
    if username.is_empty()
        || username.contains('/')
        || username.contains('?')
        || username.contains('#')
    {
        return None;
    }

    Some(format!("https://www.pinterest.com/{username}/"))
}

fn comments_section(extra: &Map<String, Value>) -> String {
    let total_count = extra.get("public_comment_count").and_then(Value::as_u64);
    let comments = extra
        .get("public_comments")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if comments.is_empty() && total_count.unwrap_or(0) == 0 {
        return String::new();
    }

    // Show when Pinterest reports more comments than the scraper imported. This
    // makes MAX_PIN_COMMENTS truncation visible without creating a huge note.
    let label = match (total_count, comments.len()) {
        (Some(total), scraped) if scraped > 0 && total > scraped as u64 => {
            format!("{scraped} scraped of {total}")
        }
        (Some(total), 0) => total.to_string(),
        (Some(total), _) => total.to_string(),
        (None, scraped) => scraped.to_string(),
    };

    let mut markup = format!(
        "<div><b>{}:</b> {}</div>\n",
        encode_safe("Pinterest comments"),
        encode_safe(&label)
    );
    for comment in comments.iter().filter_map(Value::as_object) {
        if let Some(row) = comment_row(comment) {
            markup.push_str(&row);
            markup.push('\n');
        }
    }
    markup
}

fn comment_row(comment: &Map<String, Value>) -> Option<String> {
    let text = extra_string(comment, "text")?.trim();
    if text.is_empty() {
        return None;
    }

    let mut metadata = Vec::new();
    if let Some(author) = comment_author_markup(comment) {
        metadata.push(author);
    }
    if let Some(created_at) = extra_string(comment, "created_at") {
        metadata.push(encode_safe(created_at).to_string());
    }
    let label_text = if extra_string(comment, "parent_comment_id").is_some() {
        "Reply"
    } else {
        "Comment"
    };
    let label = if metadata.is_empty() {
        format!("<b>{label_text}</b>")
    } else {
        format!("<b>{label_text}</b> ({})", metadata.join(", "))
    };
    let body = text
        .lines()
        .map(|line| encode_safe(line.trim()).to_string())
        .collect::<Vec<_>>()
        .join("<br/>");

    Some(format!("<div>{label}: {body}</div>"))
}

fn comment_author_markup(comment: &Map<String, Value>) -> Option<String> {
    if let Some(username) = extra_string(comment, "user_username") {
        let username = username.trim().trim_start_matches('@');
        if !username.is_empty() {
            let display = format!("@{username}");
            return match comment_user_url(comment)
                .or_else(|| pinterest_profile_url_for_username(username))
            {
                Some(url) => Some(link_text(&url, &display)),
                None => Some(encode_safe(&display).to_string()),
            };
        }
    }

    if let Some(full_name) = extra_string(comment, "user_full_name") {
        let full_name = full_name.trim();
        if !full_name.is_empty() {
            return match comment_user_url(comment) {
                Some(url) => Some(link_text(&url, full_name)),
                None => Some(encode_safe(full_name).to_string()),
            };
        }
    }

    // Public scraping sometimes returns only a numeric user id. That id is not a
    // username and the public site does not expose a reliable id-to-profile lookup.
    None
}

fn comment_user_url(comment: &Map<String, Value>) -> Option<String> {
    extra_string(comment, "user_url").and_then(pinterest_url)
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
        .map(|url| link_text_field(label, url, url))
        .unwrap_or_default()
}

fn imported_by_field() -> String {
    format!(
        "<div>Imported by <a href=\"{href}\">{text}</a></div>",
        href = encode_double_quoted_attribute(PROJECT_URL),
        text = encode_safe(PROJECT_URL)
    )
}

fn link_text_field(label: &str, url: &str, text: &str) -> String {
    let link = link_text(url, text);
    format!("<div><b>{}:</b> {link}</div>", encode_safe(label))
}

fn link_text(url: &str, text: &str) -> String {
    let href = encode_double_quoted_attribute(url);
    let text = encode_safe(text);
    format!("<a href=\"{href}\">{text}</a>")
}

fn clean_title_without_hashtags(raw: &str) -> Option<String> {
    let ranges = hashtag_ranges(raw);
    let mut cleaned = String::with_capacity(raw.len());
    let mut offset = 0;

    for (range, _) in ranges {
        cleaned.push_str(&raw[offset..range.start]);
        offset = range.end;
    }
    cleaned.push_str(&raw[offset..]);

    clean_title_separators(&cleaned)
}

fn hashtags_from_text(raw: &str) -> Vec<String> {
    let mut tags = Vec::new();
    for (_, tag) in hashtag_ranges(raw) {
        if !tags.iter().any(|existing| existing == &tag) {
            tags.push(tag);
        }
    }
    tags
}

fn hashtag_ranges(raw: &str) -> Vec<(std::ops::Range<usize>, String)> {
    // Return byte ranges so callers can remove tags from the original UTF-8 string
    // without corrupting non-ASCII titles.
    let positions = raw.char_indices().collect::<Vec<_>>();
    let mut ranges = Vec::new();
    let mut index = 0;

    while index < positions.len() {
        let (start, character) = positions[index];
        if character != '#' {
            index += 1;
            continue;
        }

        let mut end_index = index + 1;
        let mut tag = String::new();
        while end_index < positions.len() {
            let (_, character) = positions[end_index];
            if !is_hashtag_character(character) {
                break;
            }
            tag.extend(character.to_lowercase());
            end_index += 1;
        }

        if tag.is_empty() {
            index += 1;
            continue;
        }

        let end = positions
            .get(end_index)
            .map(|(offset, _)| *offset)
            .unwrap_or(raw.len());
        ranges.push((start..end, tag));
        index = end_index;
    }

    ranges
}

fn is_hashtag_character(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
}

fn clean_title_separators(raw: &str) -> Option<String> {
    let cleaned = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let cleaned = cleaned
        .trim_matches(|character| matches!(character, '|' | '-' | ',' | ':' | ';' | '/' | '\\'))
        .trim();

    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.to_string())
    }
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
    use std::collections::BTreeMap;

    use serde_json::{Map, Value};

    use super::*;
    use crate::pinterest::{PinterestBoard, PinterestBoardSection, PinterestMedia, PinterestPin};

    fn saved_pin_with_title(title: &str) -> SavedPin {
        SavedPin {
            pin: PinterestPin {
                id: "123".to_string(),
                title: Some(title.to_string()),
                description: None,
                link: None,
                created_at: None,
                board_id: None,
                board_section_id: None,
                board_owner: None,
                parent_pin_id: None,
                alt_text: None,
                creative_type: None,
                media: None,
                extra: Map::new(),
            },
            board: None,
            section: None,
        }
    }

    #[test]
    fn extracts_hashtags_from_title_and_drops_them_from_title() {
        let saved = saved_pin_with_title(
            "#olderbrothercore #olderbrother #nostalgia | Y2k older brother core wallpaper, Older brother corr, Nostalgic",
        );

        assert_eq!(
            title(&saved),
            "Y2k older brother core wallpaper, Older brother corr, Nostalgic"
        );
        assert_eq!(
            title_hashtags(&saved),
            vec![
                "olderbrothercore".to_string(),
                "olderbrother".to_string(),
                "nostalgia".to_string(),
            ]
        );

        let enml = enml(&saved, None);
        assert!(!enml.contains("Y2k older brother core wallpaper"));
        assert!(!enml.contains("#olderbrothercore"));
        assert!(!enml.contains("#olderbrother"));
        assert!(!enml.contains("#nostalgia"));
    }

    #[test]
    fn title_falls_back_and_truncates() {
        assert_eq!(title(&saved_pin_with_title("#tag | ---")), "Pinterest pin");

        let long_title = "a".repeat(300);
        let title = title(&saved_pin_with_title(&long_title));

        assert_eq!(title.chars().count(), 250);
        assert!(title.ends_with("..."));
    }

    #[test]
    fn omits_zero_comment_count() {
        let mut saved = saved_pin_with_title("No comments");
        saved
            .pin
            .extra
            .insert("public_comment_count".to_string(), Value::from(0));

        let enml = enml(&saved, None);

        assert!(!enml.contains("Pinterest comments"));
    }

    #[test]
    fn omits_comment_user_id_when_username_is_missing() {
        let mut comment = Map::new();
        comment.insert(
            "text".to_string(),
            Value::String("No public username".to_string()),
        );
        comment.insert(
            "user_id".to_string(),
            Value::String("123456789".to_string()),
        );
        comment.insert(
            "created_at".to_string(),
            Value::String("Thu, 21 Nov 2024 18:26:37 +0000".to_string()),
        );

        let row = comment_row(&comment).expect("comment row");

        assert!(row.contains("No public username"));
        assert!(row.contains("Thu, 21 Nov 2024 18:26:37 +0000"));
        assert!(!row.contains("123456789"));
        assert!(!row.contains("Pinterest user"));
    }

    #[test]
    fn renders_comment_summary_and_full_name_author() {
        let mut extra = Map::new();
        extra.insert("public_comment_count".to_string(), Value::from(3));
        extra.insert(
            "public_comments".to_string(),
            Value::Array(vec![Value::Object({
                let mut comment = Map::new();
                comment.insert(
                    "text".to_string(),
                    Value::String(" Line 1 <tag>\nLine 2 ".to_string()),
                );
                comment.insert(
                    "user_full_name".to_string(),
                    Value::String("Full <Name>".to_string()),
                );
                comment.insert(
                    "user_url".to_string(),
                    Value::String("/full-name/".to_string()),
                );
                comment
            })]),
        );

        let markup = comments_section(&extra);

        assert!(markup.contains("1 scraped of 3"));
        assert!(markup.contains("Line 1 &lt;tag&gt;<br/>Line 2"));
        assert!(markup.contains("Full &lt;Name&gt;"));
        assert!(markup.contains("https://www.pinterest.com/full-name/"));
    }

    #[test]
    fn ignores_empty_comments() {
        let mut comment = Map::new();
        comment.insert("text".to_string(), Value::String("   ".to_string()));

        assert!(comment_row(&comment).is_none());
    }

    #[test]
    fn renders_comment_replies_without_parent_id() {
        let mut comment = Map::new();
        comment.insert(
            "text".to_string(),
            Value::String("Nested reply".to_string()),
        );
        comment.insert(
            "parent_comment_id".to_string(),
            Value::String("2916023393059841600".to_string()),
        );

        let row = comment_row(&comment).expect("comment row");

        assert!(row.contains("<b>Reply</b>"));
        assert!(row.contains("Nested reply"));
        assert!(!row.contains("2916023393059841600"));
    }

    #[test]
    fn rejects_invalid_pinterest_profile_usernames() {
        assert_eq!(
            pinterest_profile_url_for_username("valid_user").as_deref(),
            Some("https://www.pinterest.com/valid_user/")
        );
        assert!(pinterest_profile_url_for_username("bad/user").is_none());
        assert!(pinterest_profile_url_for_username("bad?user").is_none());
        assert!(pinterest_profile_url_for_username("bad#user").is_none());
    }

    #[test]
    fn renders_image_media_and_section_id() {
        let saved = SavedPin {
            pin: PinterestPin {
                id: "123".to_string(),
                title: Some("Image pin".to_string()),
                description: None,
                link: None,
                created_at: None,
                board_id: None,
                board_section_id: Some("section-1".to_string()),
                board_owner: None,
                parent_pin_id: None,
                alt_text: None,
                creative_type: None,
                media: Some(PinterestMedia {
                    media_type: Some("image".to_string()),
                    images: BTreeMap::new(),
                    url: Some("https://i.pinimg.com/originals/example.jpg".to_string()),
                    extra: Map::new(),
                }),
                extra: Map::new(),
            },
            board: None,
            section: Some(PinterestBoardSection {
                id: "section-1".to_string(),
                name: None,
                extra: Map::new(),
            }),
        };
        let image = DownloadedImage {
            source_url: "https://i.pinimg.com/originals/example.jpg".to_string(),
            bytes: vec![1, 2, 3],
            mime_type: "image/jpeg".to_string(),
            hash: vec![0xab, 0xcd],
            hash_hex: "abcd".to_string(),
            file_name: "example.jpg".to_string(),
        };

        let enml = enml(&saved, Some(&image));

        assert!(enml.contains("<en-media type=\"image/jpeg\" hash=\"abcd\" />"));
        assert!(enml.contains("<b>Section:</b> section-1"));
        assert!(enml.contains("https://i.pinimg.com/originals/example.jpg"));
    }

    #[test]
    fn renders_enml_with_escaped_values() {
        let mut extra = Map::new();
        extra.insert(
            "public_author".to_string(),
            Value::String("Author <Name>".to_string()),
        );
        extra.insert(
            "public_author_username".to_string(),
            Value::String("author_user".to_string()),
        );
        extra.insert("public_comment_count".to_string(), Value::from(2));
        extra.insert(
            "public_comments".to_string(),
            Value::Array(vec![Value::Object({
                let mut comment = Map::new();
                comment.insert("text".to_string(), Value::String("Nice <pin>".to_string()));
                comment.insert(
                    "created_at".to_string(),
                    Value::String("Mon, 15 Jun 2026 10:00:00 +0000".to_string()),
                );
                comment.insert("user_id".to_string(), Value::String("user-1".to_string()));
                comment.insert(
                    "user_username".to_string(),
                    Value::String("commenter".to_string()),
                );
                comment.insert(
                    "user_full_name".to_string(),
                    Value::String("Commenter Name".to_string()),
                );
                comment.insert(
                    "user_url".to_string(),
                    Value::String("/commenter/".to_string()),
                );
                comment
            })]),
        );
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
                extra,
            },
            board: Some(PinterestBoard {
                id: "board-1".to_string(),
                name: Some("Ideas".to_string()),
                extra: Map::from_iter([(
                    "url".to_string(),
                    Value::String("/vitalyzdanevich/ideas/".to_string()),
                )]),
            }),
            section: None,
        };

        let enml = enml(&saved, None);

        assert!(!enml.contains("Pin ID"));
        assert!(!enml.contains("Pinterest pin"));
        assert!(!enml.contains("Board owner"));
        assert!(!enml.contains("<b>Title:</b>"));
        assert!(!enml.contains("<b>Created at:</b>"));
        assert!(!enml.contains("Creative type"));
        assert!(!enml.contains("A &lt; B"));
        assert!(enml.contains("Line &amp; 2"));
        assert!(enml.contains("https://example.com/?a=1&amp;b=2"));
        assert!(enml.contains("Alt &gt; text"));
        assert!(enml.contains("Ideas"));
        assert!(enml.contains("https://www.pinterest.com/vitalyzdanevich/ideas/"));
        assert!(enml.contains("Author &lt;Name&gt;"));
        assert!(enml.contains("https://www.pinterest.com/author_user/"));
        assert!(enml.contains("Pinterest comments"));
        assert!(enml.contains("Nice &lt;pin&gt;"));
        assert!(enml.contains("@commenter"));
        assert!(enml.contains("https://www.pinterest.com/commenter/"));
        assert!(!enml.contains("Pinterest user user-1"));
        assert!(enml.contains("Imported by"));
        assert!(enml.contains(PROJECT_URL));
    }
}
