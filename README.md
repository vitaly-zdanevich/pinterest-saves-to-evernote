# Pinterest Saves to Evernote

Rust CLI for a scheduled GitHub Actions job that exports newly saved Pinterest pins to Evernote.

This exists because Pinterest saves are not durable archives: Pins and their source images can disappear when Pinterest, the uploader, or the source site removes them. The sync creates one Evernote note per new saved Pin and can attach the image itself, so your saved reference survives outside Pinterest.

The default first run is intentionally a baseline run: it records all currently visible Pinterest pins in `state/state.json` and creates no Evernote notes. Later runs create one Evernote note per new pin only. Set `BACKFILL_EXISTING=true` only if you deliberately want to import existing history.

Each note includes the pin title, description, alt text, Pinterest link, source link, image URL, board metadata, and the image itself when Pinterest returns a downloadable image URL. Pinterest API v5 does not expose pin comments, so comments are not exported.

## GitHub Actions Schedule

The workflow in `.github/workflows/sync.yml` runs once per day at `03:17 UTC`. It can also be started manually from the GitHub Actions tab.

The `sync` job keeps `state/state.json` in the GitHub Actions cache and uploads it as a workflow artifact after each run. This is enough for one personal scheduled job; do not run multiple schedules for the same Pinterest/Evernote account in parallel.

The workflow uses Node.js 24-compatible action majors: `actions/checkout@v6`, `actions/cache@v5`, and `actions/upload-artifact@v6`.

## Required GitHub Secrets

Use refresh-token mode for unattended runs:

- `PINTEREST_CLIENT_ID`
- `PINTEREST_CLIENT_SECRET`
- `PINTEREST_REFRESH_TOKEN`
- `EVERNOTE_AUTH_TOKEN`

Pinterest token links:

- Pinterest currently requires a business account to create developer apps. Convert your existing Pinterest account from [Account settings](https://www.pinterest.com/settings/account-settings/) with `Convert account`; this should be the same account that contains the saved Pins you want to sync.
- Create/register a Pinterest app from [Pinterest Developers - My apps](https://developers.pinterest.com/apps/).
- Follow [Set up authentication and authorization](https://developers.pinterest.com/docs/getting-started/set-up-authentication-and-authorization/) to get the app ID/client ID, client secret, authorization code, access token, and refresh token.
- Request the scopes `boards:read,pins:read`. If you need secret boards, also request `boards:read_secret,pins:read_secret`.
- Use the returned `pinr...` value as `PINTEREST_REFRESH_TOKEN`; this tool refreshes the short-lived `pina...` access token automatically during each run.
- Optional: use Pinterest's [Token Debugger](https://developers.pinterest.com/docs/developer-tools/token-debugger/) to inspect token validity and scopes.

Optional direct Pinterest access token:

- `PINTEREST_ACCESS_TOKEN`

Optional Evernote variables:

- `EVERNOTE_NOTE_STORE_URL`: if omitted, the tool resolves it through Evernote UserStore.
- `EVERNOTE_NOTEBOOK_GUID`: target notebook GUID. If omitted, Evernote uses the default notebook.

These optional Evernote values can be stored as GitHub Actions secrets too.

## Optional GitHub Variables

- `EVERNOTE_TAGS`: comma-separated tags. Defaults to `pinterest`.

Optional Pinterest behavior:

- `PINTEREST_BOARD_IDS`: comma-separated board IDs. If omitted, the tool lists all boards and then pins on each board.
- `PINTEREST_FETCH_MODE`: `boards` or `account`. Defaults to `boards`.
- `PINTEREST_INCLUDE_SECTIONS`: also list board-section pins. Defaults to `true`.
- `PINTEREST_API_BASE_URL`: defaults to `https://api.pinterest.com/v5`.
- `BACKFILL_EXISTING`: import existing pins on first run. Defaults to `false`.
- `MAX_PINS_PER_RUN`: cap Evernote notes per run. Defaults to `25`.
- `ATTACH_IMAGES`: download and attach the image resource to Evernote. Defaults to `true`.
- `MAX_IMAGE_BYTES`: maximum image download size. Defaults to `26214400`.
- `DRY_RUN`: fetch and log without writing Evernote or state. Defaults to `false`.

The GitHub workflow uses `STATE_PATH=state/state.json` because the Actions cache is configured for the `state/` directory.

## Pinterest API

The tool uses Pinterest API v5 with `boards:read` and `pins:read`. It reads boards with `GET /v5/boards`, saved pins with `GET /v5/boards/{board_id}/pins`, optional section pins with `GET /v5/boards/{board_id}/sections/{section_id}/pins`, or account pins with `GET /v5/pins` when `PINTEREST_FETCH_MODE=account`.

Available pin metadata depends on Pinterest's response, but the code handles the common API fields: `id`, `title`, `description`, `link`, `created_at`, `board_id`, `board_section_id`, `board_owner.username`, `parent_pin_id`, `alt_text`, `creative_type`, and `media.images`.

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
