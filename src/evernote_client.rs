use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use evernote::note_store::{NoteStoreSyncClient, TNoteStoreSyncClient};
use evernote::types::{self, NoteAttributes, ResourceAttributes};
use evernote::user_store::{
    EDAM_VERSION_MAJOR, EDAM_VERSION_MINOR, TUserStoreSyncClient, UserStoreSyncClient,
};
use reqwest::blocking::Client as ReqwestClient;
use thrift::protocol::{TBinaryInputProtocol, TBinaryOutputProtocol};
use thrift::transport::{ReadHalf, TIoChannel, WriteHalf};

use crate::image::DownloadedImage;

const CLIENT_NAME: &str = "pinterest-saves-to-evernote/0.1";
const DEFAULT_USER_STORE_URL: &str = "https://www.evernote.com/edam/user";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

type InputProtocol<C> = TBinaryInputProtocol<ReadHalf<ThriftHttpChannel<C>>>;
type OutputProtocol<C> = TBinaryOutputProtocol<WriteHalf<ThriftHttpChannel<C>>>;

pub trait ThriftHttpClient: Clone + Send + Sync + 'static {
    fn post_thrift(&self, url: &str, body: Vec<u8>) -> Result<Vec<u8>, String>;
}

#[derive(Clone)]
pub struct ReqwestThriftHttpClient {
    client: ReqwestClient,
}

impl ReqwestThriftHttpClient {
    pub fn new() -> Result<Self> {
        let client = ReqwestClient::builder()
            .user_agent(CLIENT_NAME)
            .timeout(REQUEST_TIMEOUT)
            .pool_max_idle_per_host(2)
            .build()
            .context("failed to build Evernote HTTP client")?;
        Ok(Self { client })
    }
}

impl ThriftHttpClient for ReqwestThriftHttpClient {
    fn post_thrift(&self, url: &str, body: Vec<u8>) -> Result<Vec<u8>, String> {
        let response = self
            .client
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/x-thrift")
            .body(body)
            .send()
            .map_err(|error| format!("Evernote request failed: {error}"))?
            .error_for_status()
            .map_err(|error| format!("Evernote returned an HTTP error: {error}"))?;

        response
            .bytes()
            .map(|bytes| bytes.to_vec())
            .map_err(|error| format!("failed to read Evernote response: {error}"))
    }
}

#[derive(Clone)]
pub struct EvernoteClient<C = ReqwestThriftHttpClient>
where
    C: ThriftHttpClient,
{
    token: String,
    user_store_url: String,
    note_store_url: Arc<Mutex<Option<String>>>,
    notebook_guid: Option<String>,
    tags: Vec<String>,
    http: C,
}

impl EvernoteClient<ReqwestThriftHttpClient> {
    pub fn new(
        token: impl Into<String>,
        user_store_url: Option<String>,
        note_store_url: Option<String>,
        notebook_guid: Option<String>,
        tags: Vec<String>,
    ) -> Result<Self> {
        Ok(Self::with_http_client(
            token,
            user_store_url.unwrap_or_else(|| DEFAULT_USER_STORE_URL.to_string()),
            note_store_url,
            notebook_guid,
            tags,
            ReqwestThriftHttpClient::new()?,
        ))
    }
}

impl<C> EvernoteClient<C>
where
    C: ThriftHttpClient,
{
    pub fn with_http_client(
        token: impl Into<String>,
        user_store_url: impl Into<String>,
        note_store_url: Option<String>,
        notebook_guid: Option<String>,
        tags: Vec<String>,
        http: C,
    ) -> Self {
        Self {
            token: token.into(),
            user_store_url: user_store_url.into(),
            note_store_url: Arc::new(Mutex::new(note_store_url)),
            notebook_guid,
            tags,
            http,
        }
    }

    pub fn create_pin_note(
        &self,
        title: String,
        content: String,
        image: Option<&DownloadedImage>,
        source_url: String,
    ) -> Result<String> {
        let mut client = self.note_store_client()?;
        let resources = image
            .map(|image| vec![image_resource(image)])
            .filter(|v| !v.is_empty());
        let note = types::Note {
            title: Some(title),
            content: Some(content),
            notebook_guid: self.notebook_guid.clone(),
            resources,
            attributes: Some(NoteAttributes {
                source: Some("pinterest".to_string()),
                source_u_r_l: Some(source_url),
                source_application: Some(CLIENT_NAME.to_string()),
                ..NoteAttributes::default()
            }),
            tag_names: Some(self.tags.clone()),
            ..types::Note::default()
        };

        let created = client
            .create_note(self.token.clone(), note)
            .map_err(|error| anyhow!("Evernote API error: {error}"))?;

        created
            .guid
            .context("Evernote did not return a GUID for the created note")
    }

    fn user_store_client(
        &self,
    ) -> Result<UserStoreSyncClient<InputProtocol<C>, OutputProtocol<C>>> {
        let channel = ThriftHttpChannel::new(self.user_store_url.clone(), self.http.clone());
        let (read, write) = channel.split().map_err(|error| {
            anyhow!("failed to initialize Evernote UserStore transport: {error}")
        })?;
        Ok(UserStoreSyncClient::new(
            TBinaryInputProtocol::new(read, true),
            TBinaryOutputProtocol::new(write, true),
        ))
    }

    fn note_store_client(
        &self,
    ) -> Result<NoteStoreSyncClient<InputProtocol<C>, OutputProtocol<C>>> {
        let note_store_url = self.note_store_url()?;
        let channel = ThriftHttpChannel::new(note_store_url, self.http.clone());
        let (read, write) = channel.split().map_err(|error| {
            anyhow!("failed to initialize Evernote NoteStore transport: {error}")
        })?;
        Ok(NoteStoreSyncClient::new(
            TBinaryInputProtocol::new(read, true),
            TBinaryOutputProtocol::new(write, true),
        ))
    }

    fn note_store_url(&self) -> Result<String> {
        if let Some(url) = self
            .note_store_url
            .lock()
            .map_err(|_| anyhow!("Evernote NoteStore URL cache is poisoned"))?
            .clone()
        {
            return Ok(url);
        }

        let mut client = self.user_store_client()?;
        let version_ok = client
            .check_version(
                CLIENT_NAME.to_string(),
                EDAM_VERSION_MAJOR,
                EDAM_VERSION_MINOR,
            )
            .map_err(|error| anyhow!("Evernote UserStore API error: {error}"))?;
        if !version_ok {
            return Err(anyhow!("Evernote EDAM protocol version is not supported"));
        }

        let urls = client
            .get_user_urls(self.token.clone())
            .map_err(|error| anyhow!("Evernote UserStore API error: {error}"))?;
        let note_store_url = urls
            .note_store_url
            .filter(|url| !url.trim().is_empty())
            .ok_or_else(|| anyhow!("Evernote did not return a NoteStore URL"))?;

        *self
            .note_store_url
            .lock()
            .map_err(|_| anyhow!("Evernote NoteStore URL cache is poisoned"))? =
            Some(note_store_url.clone());
        Ok(note_store_url)
    }
}

fn image_resource(image: &DownloadedImage) -> types::Resource {
    types::Resource {
        data: Some(types::Data {
            body_hash: Some(image.hash.clone()),
            size: Some(image.bytes.len().min(i32::MAX as usize) as i32),
            body: Some(image.bytes.clone()),
        }),
        mime: Some(image.mime_type.clone()),
        active: Some(true),
        attributes: Some(ResourceAttributes {
            source_u_r_l: Some(image.source_url.clone()),
            file_name: Some(image.file_name.clone()),
            attachment: Some(false),
            ..ResourceAttributes::default()
        }),
        ..types::Resource::default()
    }
}

#[derive(Clone)]
struct ThriftHttpChannel<C>
where
    C: ThriftHttpClient,
{
    endpoint: String,
    http: C,
    state: Arc<Mutex<ThriftHttpState>>,
}

#[derive(Default)]
struct ThriftHttpState {
    read_bytes: Vec<u8>,
    read_pos: usize,
    write_bytes: Vec<u8>,
}

impl<C> ThriftHttpChannel<C>
where
    C: ThriftHttpClient,
{
    fn new(endpoint: String, http: C) -> Self {
        Self {
            endpoint,
            http,
            state: Arc::new(Mutex::new(ThriftHttpState::default())),
        }
    }
}

impl<C> TIoChannel for ThriftHttpChannel<C>
where
    C: ThriftHttpClient,
{
    fn split(self) -> thrift::Result<(ReadHalf<Self>, WriteHalf<Self>)>
    where
        Self: Sized,
    {
        Ok((ReadHalf::new(self.clone()), WriteHalf::new(self)))
    }
}

impl<C> Read for ThriftHttpChannel<C>
where
    C: ThriftHttpClient,
{
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("Evernote transport state is poisoned"))?;
        let remaining = state.read_bytes.len().saturating_sub(state.read_pos);
        let read_len = remaining.min(buf.len());
        if read_len == 0 {
            return Ok(0);
        }

        let start = state.read_pos;
        let end = start + read_len;
        buf[..read_len].copy_from_slice(&state.read_bytes[start..end]);
        state.read_pos = end;
        Ok(read_len)
    }
}

impl<C> Write for ThriftHttpChannel<C>
where
    C: ThriftHttpClient,
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("Evernote transport state is poisoned"))?;
        state.write_bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let request_body = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| io::Error::other("Evernote transport state is poisoned"))?;
            std::mem::take(&mut state.write_bytes)
        };

        let response_body = self
            .http
            .post_thrift(&self.endpoint, request_body)
            .map_err(io::Error::other)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("Evernote transport state is poisoned"))?;
        state.read_bytes = response_body;
        state.read_pos = 0;
        Ok(())
    }
}
