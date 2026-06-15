# Privacy Policy

Effective date: June 15, 2026

Pinterest Saves to Evernote is an open-source personal sync tool. It reads Pinterest data from the account you configure and creates Evernote notes in the Evernote account you configure.

## Data Accessed

When you run this tool, it may access:

- Pinterest account, board, and Pin metadata, including titles, descriptions, image URLs, source links, board names, author information, and save times.
- Pinterest images, when image attachment is enabled.
- Public Pinterest Pin comments, when comment scraping is enabled.
- Evernote account metadata needed to create notes and find the selected notebook.

## How Data Is Used

The data is used only to create Evernote notes from newly saved Pinterest Pins. One Evernote note is created per new Pin.

This project does not sell data, use data for advertising, or send data to any analytics service.

## Storage

This project does not operate a hosted server.

If you run it through GitHub Actions, credentials are stored in your own GitHub repository secrets, and sync state is stored in your own GitHub Actions cache or artifacts. The sync state contains processed Pinterest Pin IDs and timestamps so the tool can avoid creating duplicate notes.

Pinterest and Evernote tokens may also be stored locally if you run the tool on your own machine.

## Sharing

Data is sent only to services required for the sync:

- Pinterest, to read Pins and related metadata.
- Evernote, to create notes.
- GitHub Actions, if you choose to run the scheduled workflow there.

## Deleting Data and Revoking Access

You can stop future access by deleting the configured GitHub Actions secrets, deleting local environment files, or revoking the Pinterest and Evernote tokens in those services.

You can delete generated Evernote notes from your Evernote account. You can delete sync state by removing `state/state.json`, GitHub Actions cache entries, and uploaded state artifacts.

## Contact

For questions or issues, use the GitHub repository:

https://github.com/vitaly-zdanevich/pinterest-saves-to-evernote
