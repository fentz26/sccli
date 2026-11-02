//! In-app search, playlist browsing and queue filtering.
use super::*;

impl App {
    pub(super) fn visible_tracks(&self) -> Vec<usize> {
        self.tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| browser::matches(&t.title, &self.queue_filter.text))
            .map(|(i, _)| i)
            .collect()
    }

    pub(super) fn reconcile_filter(&mut self) {
        let visible = self.visible_tracks();
        if !visible.contains(&self.sel) {
            self.sel = visible.first().copied().unwrap_or(self.cur);
        }
        self.top = 0;
        self.hover = None;
    }

    pub(super) fn saved_playlists(&self) -> Vec<Playlist> {
        self.cfg
            .aliases
            .iter()
            .map(|(name, url)| Playlist {
                title: format!(
                    "{} · {}",
                    name.chars().filter(|c| !c.is_control()).collect::<String>(),
                    tr(K::SavedShortcut)
                ),
                url: url.clone(),
            })
            .collect()
    }

    pub(super) fn refresh_playlists(&mut self) {
        if self.playlists.loading {
            return;
        }
        self.playlists.items = self.saved_playlists();
        self.playlists.error = None;
        let Some(user) = self.cfg.user.clone() else {
            self.playlists.loaded = true;
            return;
        };
        self.playlists.loading = true;
        let api = self.api.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = api.playlists(&user).map_err(|e| e.to_string());
            let _ = tx.send(Ev::PlaylistsDone(result));
        });
    }

    pub(super) fn start_search(&mut self) {
        let query = self.search.input.text.trim().to_owned();
        if query.is_empty() {
            return;
        }
        self.search.query = query.clone();
        self.search.tracks.clear();
        self.search.error = None;
        self.search.loading = true;
        self.playlists.opening = false;
        let id = self.browse_epoch.fetch_add(1, Ordering::Relaxed) + 1;
        let cancel = Cancel::tied_to(&self.browse_epoch);
        let api = self.api.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = api.search(&query, &cancel).map_err(|e| e.to_string());
            let _ = tx.send(Ev::SearchDone(id, query, result));
        });
    }

    pub(super) fn open_playlist(&mut self) {
        let Some(item) = self.playlists.items.get(self.playlists.selected) else {
            return;
        };
        let source = match crate::resolve_url_str(&item.url, &self.cfg) {
            Ok(source) => source,
            Err(error) => {
                self.playlists.error = Some(error.to_string());
                return;
            }
        };
        let id = self.browse_epoch.fetch_add(1, Ordering::Relaxed) + 1;
        self.search.loading = false;
        self.playlists.opening = true;
        self.playlists.error = None;
        let api = self.api.clone();
        let token = self.cfg.token.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let mut tracks = Vec::new();
            let native = api.list(&source, &mut |page| tracks.extend(page));
            let result = match native {
                Ok(true) => Ok(tracks),
                Err(ref error)
                    if error
                        .downcast_ref::<ApiError>()
                        .is_some_and(|e| e.is_transient() || matches!(e, ApiError::Auth | ApiError::Cancelled)) =>
                {
                    Err(error.to_string())
                }
                _ if tracks.is_empty() => {
                    match api::ytdlp_list(&source, token.as_deref(), &mut |page| tracks.extend(page)) {
                        None => Ok(tracks),
                        Some(error) => Err(error),
                    }
                }
                Err(error) => Err(error.to_string()),
                Ok(false) => Err(tr(K::NoTracks).into()),
            };
            let _ = tx.send(Ev::PlaylistDone(id, source, result));
        });
    }

    pub(super) fn activate_source(&mut self, source: String, tracks: Vec<Track>, index: usize) {
        if tracks.is_empty() || index >= tracks.len() {
            return;
        }
        self.list_generation += 1; // late initial listing pages belong to the old queue
        self.source = source;
        self.tracks = tracks;
        self.list_done = true;
        self.shuffle_pending = false;
        self.shuffled = false;
        self.resume = None;
        self.want = None;
        self.inflight.clear();
        self.queue_filter = Input::default();
        self.sel = index;
        self.top = 0;
        self.back = false;
        self.note.clear();
        self.go(index);
        self.set_view(View::Queue);
    }

    pub(super) fn search_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        if self.search.input.editing {
            let submit = code == KeyCode::Enter;
            self.search.input.key(code, mods);
            if submit {
                self.start_search();
            }
            return;
        }
        match code {
            KeyCode::Char('/') => self.search.input.editing = true,
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('l') => self.set_view(View::Player),
            KeyCode::Up | KeyCode::Char('k') => self.search.selected = self.search.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.search.selected = (self.search.selected + 1).min(self.search.tracks.len().saturating_sub(1))
            }
            KeyCode::Home => self.search.selected = 0,
            KeyCode::End => self.search.selected = self.search.tracks.len().saturating_sub(1),
            KeyCode::Enter if !self.search.loading => {
                self.activate_source(
                    format!("scsearch25:{}", self.search.query),
                    self.search.tracks.clone(),
                    self.search.selected,
                );
            }
            _ => self.play_key(code, mods),
        }
    }

    pub(super) fn playlists_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        match code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('l') => self.set_view(View::Player),
            KeyCode::Char('r') => self.refresh_playlists(),
            KeyCode::Up | KeyCode::Char('k') => self.playlists.selected = self.playlists.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.playlists.selected =
                    (self.playlists.selected + 1).min(self.playlists.items.len().saturating_sub(1))
            }
            KeyCode::Home => self.playlists.selected = 0,
            KeyCode::End => self.playlists.selected = self.playlists.items.len().saturating_sub(1),
            KeyCode::Enter => self.open_playlist(),
            _ => self.play_key(code, mods),
        }
    }

    pub(super) fn search_rows(&mut self, w: usize, rows: usize, out: &mut Vec<String>) {
        let hint = if self.search.input.editing {
            tr(K::SearchPrompt).replace("{q}", &self.search.input.text)
        } else {
            tr(K::SearchHint).replace("{q}", &self.search.query)
        };
        out.push(clip(&hint, w));
        if rows == 0 {
            return;
        }
        if self.search.loading {
            out.push(clip(tr(K::Loading), w));
        } else if let Some(error) = &self.search.error {
            out.push(clip(error, w));
        } else if self.search.tracks.is_empty() {
            out.push(clip(tr(K::NoMatches), w));
        } else {
            browser::scroll(
                &mut self.search.top,
                self.search.selected,
                self.search.tracks.len(),
                rows,
            );
            for (i, track) in self.search.tracks.iter().enumerate().skip(self.search.top).take(rows) {
                let row = clip(&format!("   {}", track.title), w);
                out.push(if i == self.search.selected { reverse(&row) } else { row });
            }
        }
    }

    pub(super) fn playlists_rows(&mut self, w: usize, rows: usize, out: &mut Vec<String>) {
        out.push(clip(tr(K::PlaylistHint), w));
        if rows == 0 {
            return;
        }
        // Keep saved shortcuts usable even if the remote library cannot load.
        let notice = if let Some(error) = &self.playlists.error {
            Some(error.as_str())
        } else if self.playlists.loading || self.playlists.opening {
            Some(tr(K::Loading))
        } else if self.playlists.items.is_empty() {
            Some(if self.cfg.user.is_none() {
                tr(K::NotSignedIn)
            } else {
                tr(K::NoPlaylists)
            })
        } else {
            None
        };
        let available = rows.saturating_sub(usize::from(notice.is_some() && rows > 1));
        browser::scroll(
            &mut self.playlists.top,
            self.playlists.selected,
            self.playlists.items.len(),
            available,
        );
        for (i, item) in self
            .playlists
            .items
            .iter()
            .enumerate()
            .skip(self.playlists.top)
            .take(available)
        {
            let row = clip(&format!("   {}", item.title), w);
            out.push(if i == self.playlists.selected {
                reverse(&row)
            } else {
                row
            });
        }
        if let Some(notice) = notice {
            if out.len() < rows + 2 {
                out.push(clip(notice, w));
            }
        }
    }
}
