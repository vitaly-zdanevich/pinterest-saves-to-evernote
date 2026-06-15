# Pinterest Saves to Evernote

Rust CLI for a scheduled GitHub Actions job that exports newly saved Pinterest pins to Evernote.

This exists because Pinterest saves are not durable archives: Pins and their source images can disappear when Pinterest, the uploader, or the source site removes them. The sync creates one Evernote note per new saved Pin and can attach the image itself, so your saved reference survives outside Pinterest.

The default first run is intentionally a baseline run: it records all currently visible Pinterest pins in `state/state.json` and creates no Evernote notes. Later runs create one Evernote note per new pin only. Set `BACKFILL_EXISTING=true` only if you deliberately want to import existing history.

Each note includes the pin title, description, alt text, Pinterest link, source link, image URL, board metadata, public comments when Pinterest exposes them, and the image itself when Pinterest returns a downloadable image URL. Pinterest API v5 does not expose comment bodies, so comments are fetched with the same unsupported public-page fallback used while API access is unavailable.

## GitHub Actions Schedule

The scheduled sync workflow in `.github/workflows/scheduled-sync.yml` runs hourly at minute `:19`, offset from the top of the hour to avoid GitHub Actions schedule congestion. GitHub documents that scheduled workflows can be delayed during high load, that the start of every hour is a high-load time, and that queued jobs can be dropped when load is high enough. See GitHub's [`schedule` event documentation](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#schedule). The workflow can also be started manually from the GitHub Actions tab.

The hourly schedule is conservative for GitHub Actions reliability. If you use the unsupported public-profile fallback, remember that Pinterest currently exposes only a small recent public list there, so very active accounts may need a more frequent external scheduler.

The `sync` job keeps `state/state.json` in the GitHub Actions cache and uploads it as a workflow artifact after each run. This is enough for one personal scheduled job; do not run multiple schedules for the same Pinterest/Evernote account in parallel.

The CI workflow in `.github/workflows/ci.yml` runs tests on push and pull requests. Both workflows use Node.js 24-compatible action majors: `actions/checkout@v6`, `actions/cache@v5`, and `actions/upload-artifact@v6`.

## Required GitHub Secrets

For the supported Pinterest API path, use refresh-token mode for unattended runs:

- `PINTEREST_CLIENT_ID`
- `PINTEREST_CLIENT_SECRET`
- `PINTEREST_REFRESH_TOKEN`
- `EVERNOTE_AUTH_TOKEN`

For the unsupported public-profile fallback, only `EVERNOTE_AUTH_TOKEN` is required; set `PUBLIC_PROFILE_TO_PARSE_WITHOUT_API` as a GitHub Actions variable instead of adding Pinterest API secrets.

Pinterest token links:

- Pinterest currently requires a business account to create developer apps. Convert your existing Pinterest account from [Account settings](https://www.pinterest.com/settings/account-settings/) with `Convert account`; this should be the same account that contains the saved Pins you want to sync.
- Create/register a Pinterest app from [Pinterest Developers - My apps](https://developers.pinterest.com/apps/).
- Follow [Set up authentication and authorization](https://developers.pinterest.com/docs/getting-started/set-up-authentication-and-authorization/) to get the app ID/client ID, client secret, authorization code, access token, and refresh token.
- Request the scopes `boards:read,pins:read`. If you need secret boards, also request `boards:read_secret,pins:read_secret`.
- Use the returned `pinr...` value as `PINTEREST_REFRESH_TOKEN`; this tool refreshes the short-lived `pina...` access token automatically during each run.
- Optional: use Pinterest's [Token Debugger](https://developers.pinterest.com/docs/developer-tools/token-debugger/) to inspect token validity and scopes.

Optional direct Pinterest access token:

- `PINTEREST_ACCESS_TOKEN`

Unsupported public-profile fallback while waiting for Pinterest API verification:

- Add a GitHub Actions variable named `PUBLIC_PROFILE_TO_PARSE_WITHOUT_API`.
- Set it to your public Pinterest profile or pins URL, for example `https://www.pinterest.com/vitalyzdanevich/pins/`.
- When this variable is set, the tool does not use Pinterest OAuth. It fetches the public profile HTML and parses the JSON-LD profile snippet instead.
- This currently exposes a small recent list, about 10 public pins, with pin title, image URL, Pinterest URL, original Pinterest author, and `datePublished`. It does not expose private/secret boards, full board metadata, or the original source link.
- By default, each new note also tries to scrape the public comments endpoint for that pin. When available, the note includes comment text, creation time, and the Pinterest user ID. Public responses currently do not include commenter display names.
- This is fragile and unsupported by Pinterest; keep the API credentials as the preferred long-term path.

Optional Evernote secrets:

- `EVERNOTE_NOTE_STORE_URL`: if omitted, the tool resolves it through Evernote UserStore.
- `EVERNOTE_NOTEBOOK_GUID`: target notebook GUID. Use this only if notebook-name lookup is ambiguous.

Leave `EVERNOTE_NOTEBOOK_GUID` unset when using `EVERNOTE_NOTEBOOK_NAME`.

## Optional GitHub Variables

- `EVERNOTE_TAGS`: comma-separated tags. Defaults to `pinterest`.
- `EVERNOTE_NOTEBOOK_NAME`: target Evernote notebook name. If omitted, Evernote uses the default notebook. Do not set it together with `EVERNOTE_NOTEBOOK_GUID`.

Optional Pinterest behavior:

- `PUBLIC_PROFILE_TO_PARSE_WITHOUT_API`: public Pinterest profile URL or username to parse without API access. This is an unsupported fallback and overrides the API fetch path when set.
- `PUBLIC_PROFILE_MAX_PAGES`: public-profile pages to scan by Pinterest bookmark. Defaults to `3`, and one page is usually about 25 pins. Already-initialized public-profile syncs stop at the first already processed pin so enabling pagination does not backfill older saved pins.
- `PINTEREST_BOARD_IDS`: comma-separated board IDs. If omitted, the tool lists all boards and then pins on each board.
- `PINTEREST_FETCH_MODE`: `boards` or `account`. Defaults to `boards`.
- `PINTEREST_INCLUDE_SECTIONS`: also list board-section pins. Defaults to `true`.
- `PINTEREST_API_BASE_URL`: defaults to `https://api.pinterest.com/v5`.
- `BACKFILL_EXISTING`: import existing pins on first run. Defaults to `false`.
- `MAX_PINS_PER_RUN`: cap Evernote notes per run. Defaults to `25`.
- `ATTACH_IMAGES`: download and attach the image resource to Evernote. Defaults to `true`.
- `MAX_IMAGE_BYTES`: maximum image download size. Defaults to `26214400`.
- `SCRAPE_PIN_COMMENTS`: scrape public pin comments into each new note. Defaults to `true`.
- `MAX_PIN_COMMENTS`: maximum text comments to store per note. Defaults to `25`.
- `DRY_RUN`: fetch and log without writing Evernote or state. Defaults to `false`.

The scheduled GitHub workflow uses `STATE_PATH=state/state.json` because the Actions cache is configured for the `state/` directory.

## Pinterest API

The tool uses Pinterest API v5 with `boards:read` and `pins:read`. It reads boards with `GET /v5/boards`, saved pins with `GET /v5/boards/{board_id}/pins`, optional section pins with `GET /v5/boards/{board_id}/sections/{section_id}/pins`, or account pins with `GET /v5/pins` when `PINTEREST_FETCH_MODE=account`.

Available pin metadata depends on Pinterest's response, but the code handles the common API fields: `id`, `title`, `description`, `link`, `created_at`, `board_id`, `board_section_id`, `board_owner.username`, `parent_pin_id`, `alt_text`, `creative_type`, and `media.images`.

Pinterest API v5 currently has no public endpoint for comment bodies. This project therefore scrapes comments from public pin pages by default. Treat that as best-effort archival metadata, not a stable API contract.

## Local Run

```bash
cp .env.example .env
$EDITOR .env
cargo run -- sync
```

Use dry-run mode first:

```bash
DRY_RUN=true cargo run -- sync
```
