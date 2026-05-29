use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use lsp_types::Position;
use tokio::sync::Mutex;

use crate::client::LspClient;
use crate::error::LspError;
use crate::types::{
    normalize_extension, FileDiagnostics, LspContextEnrichment, LspServerConfig, SymbolLocation,
    WorkspaceDiagnostics,
};

pub struct LspManager {
    server_configs: BTreeMap<String, LspServerConfig>,
    extension_map: BTreeMap<String, String>,
    clients: Mutex<BTreeMap<String, Arc<LspClient>>>,
}

impl LspManager {
    /// Builds a manager from the given server configurations, indexing each
    /// configured file extension to its server.
    ///
    /// # Errors
    ///
    /// Returns [`LspError::DuplicateExtension`] if two server configurations map
    /// the same (normalized) file extension to different servers.
    pub fn new(server_configs: Vec<LspServerConfig>) -> Result<Self, LspError> {
        let mut configs_by_name = BTreeMap::new();
        let mut extension_map = BTreeMap::new();

        for config in server_configs {
            for extension in config.extension_to_language.keys() {
                let normalized = normalize_extension(extension);
                if let Some(existing_server) =
                    extension_map.insert(normalized.clone(), config.name.clone())
                {
                    return Err(LspError::DuplicateExtension {
                        extension: normalized,
                        existing_server,
                        new_server: config.name.clone(),
                    });
                }
            }
            configs_by_name.insert(config.name.clone(), config);
        }

        Ok(Self {
            server_configs: configs_by_name,
            extension_map,
            clients: Mutex::new(BTreeMap::new()),
        })
    }

    #[must_use]
    pub fn supports_path(&self, path: &Path) -> bool {
        path.extension().is_some_and(|extension| {
            let normalized = normalize_extension(extension.to_string_lossy().as_ref());
            self.extension_map.contains_key(&normalized)
        })
    }

    /// Notifies the LSP server for `path` that the document has been opened with
    /// the given `text`.
    ///
    /// # Errors
    ///
    /// Returns [`LspError`] if no server is configured for the document's
    /// extension, if a server connection cannot be established, or if the
    /// underlying transport fails while sending the notification.
    pub async fn open_document(&self, path: &Path, text: &str) -> Result<(), LspError> {
        self.client_for_path(path)
            .await?
            .open_document(path, text)
            .await
    }

    /// Reads `path` from disk and pushes its current contents to the LSP server
    /// via a change notification followed by a save notification.
    ///
    /// # Errors
    ///
    /// Returns [`LspError::Io`] if the file cannot be read from disk, or any
    /// [`LspError`] surfaced while changing or saving the document (see
    /// [`Self::change_document`] and [`Self::save_document`]).
    pub async fn sync_document_from_disk(&self, path: &Path) -> Result<(), LspError> {
        let contents = std::fs::read_to_string(path)?;
        self.change_document(path, &contents).await?;
        self.save_document(path).await
    }

    /// Notifies the LSP server for `path` that the document's contents changed
    /// to `text`.
    ///
    /// # Errors
    ///
    /// Returns [`LspError`] if no server is configured for the document's
    /// extension, if a server connection cannot be established, or if the
    /// underlying transport fails while sending the notification.
    pub async fn change_document(&self, path: &Path, text: &str) -> Result<(), LspError> {
        self.client_for_path(path)
            .await?
            .change_document(path, text)
            .await
    }

    /// Notifies the LSP server for `path` that the document has been saved.
    ///
    /// # Errors
    ///
    /// Returns [`LspError`] if no server is configured for the document's
    /// extension, if a server connection cannot be established, or if the
    /// underlying transport fails while sending the notification.
    pub async fn save_document(&self, path: &Path) -> Result<(), LspError> {
        self.client_for_path(path).await?.save_document(path).await
    }

    /// Notifies the LSP server for `path` that the document has been closed.
    ///
    /// # Errors
    ///
    /// Returns [`LspError`] if no server is configured for the document's
    /// extension, if a server connection cannot be established, or if the
    /// underlying transport fails while sending the notification.
    pub async fn close_document(&self, path: &Path) -> Result<(), LspError> {
        self.client_for_path(path).await?.close_document(path).await
    }

    /// Resolves the definition(s) of the symbol at `position` in `path`,
    /// returning a de-duplicated list of locations.
    ///
    /// # Errors
    ///
    /// Returns [`LspError`] if no server is configured for the document's
    /// extension, if a server connection cannot be established, or if the
    /// underlying request to the server fails.
    pub async fn go_to_definition(
        &self,
        path: &Path,
        position: Position,
    ) -> Result<Vec<SymbolLocation>, LspError> {
        let mut locations = self
            .client_for_path(path)
            .await?
            .go_to_definition(path, position)
            .await?;
        dedupe_locations(&mut locations);
        Ok(locations)
    }

    /// Finds all references to the symbol at `position` in `path`, optionally
    /// including the declaration, returning a de-duplicated list of locations.
    ///
    /// # Errors
    ///
    /// Returns [`LspError`] if no server is configured for the document's
    /// extension, if a server connection cannot be established, or if the
    /// underlying request to the server fails.
    pub async fn find_references(
        &self,
        path: &Path,
        position: Position,
        include_declaration: bool,
    ) -> Result<Vec<SymbolLocation>, LspError> {
        let mut locations = self
            .client_for_path(path)
            .await?
            .find_references(path, position, include_declaration)
            .await?;
        dedupe_locations(&mut locations);
        Ok(locations)
    }

    /// Collects the latest diagnostics from every connected server, skipping
    /// files with no diagnostics, and returns them sorted by path.
    ///
    /// # Errors
    ///
    /// Returns [`LspError`] if querying a connected server for its diagnostics
    /// snapshot fails. URIs that cannot be parsed into file paths are skipped
    /// rather than treated as errors.
    pub async fn collect_workspace_diagnostics(&self) -> Result<WorkspaceDiagnostics, LspError> {
        let clients = self
            .clients
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut files = Vec::new();

        for client in clients {
            for (uri, diagnostics) in client.diagnostics_snapshot().await {
                let Ok(path) = url::Url::parse(&uri).and_then(|url| {
                    url.to_file_path()
                        .map_err(|()| url::ParseError::RelativeUrlWithoutBase)
                }) else {
                    continue;
                };
                if diagnostics.is_empty() {
                    continue;
                }
                files.push(FileDiagnostics {
                    path,
                    uri,
                    diagnostics,
                });
            }
        }

        files.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(WorkspaceDiagnostics { files })
    }

    /// Gathers workspace diagnostics together with the definitions and
    /// references for the symbol at `position` in `path` into a single
    /// enrichment payload.
    ///
    /// # Errors
    ///
    /// Returns [`LspError`] if collecting diagnostics, resolving definitions, or
    /// finding references fails (see [`Self::collect_workspace_diagnostics`],
    /// [`Self::go_to_definition`], and [`Self::find_references`]).
    pub async fn context_enrichment(
        &self,
        path: &Path,
        position: Position,
    ) -> Result<LspContextEnrichment, LspError> {
        Ok(LspContextEnrichment {
            file_path: path.to_path_buf(),
            diagnostics: self.collect_workspace_diagnostics().await?,
            definitions: self.go_to_definition(path, position).await?,
            references: self.find_references(path, position, true).await?,
        })
    }

    /// Shuts down every connected LSP client and clears the client cache.
    ///
    /// # Errors
    ///
    /// Returns the first [`LspError`] encountered while shutting down a client;
    /// clients are drained from the cache before shutdown is attempted, so a
    /// failure does not leave a stale client registered.
    pub async fn shutdown(&self) -> Result<(), LspError> {
        let mut clients = self.clients.lock().await;
        let drained = clients.values().cloned().collect::<Vec<_>>();
        clients.clear();
        drop(clients);

        for client in drained {
            client.shutdown().await?;
        }
        Ok(())
    }

    async fn client_for_path(&self, path: &Path) -> Result<Arc<LspClient>, LspError> {
        let extension = path
            .extension()
            .map(|extension| normalize_extension(extension.to_string_lossy().as_ref()))
            .ok_or_else(|| LspError::UnsupportedDocument(path.to_path_buf()))?;
        let server_name = self
            .extension_map
            .get(&extension)
            .cloned()
            .ok_or_else(|| LspError::UnsupportedDocument(path.to_path_buf()))?;

        let mut clients = self.clients.lock().await;
        if let Some(client) = clients.get(&server_name) {
            return Ok(client.clone());
        }

        let config = self
            .server_configs
            .get(&server_name)
            .cloned()
            .ok_or_else(|| LspError::UnknownServer(server_name.clone()))?;
        let client = Arc::new(LspClient::connect(config).await?);
        clients.insert(server_name, client.clone());
        Ok(client)
    }
}

fn dedupe_locations(locations: &mut Vec<SymbolLocation>) {
    let mut seen = BTreeSet::new();
    locations.retain(|location| {
        seen.insert((
            location.path.clone(),
            location.range.start.line,
            location.range.start.character,
            location.range.end.line,
            location.range.end.character,
        ))
    });
}
