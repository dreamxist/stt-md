//! The tray menu: what it shows and which click means what.

use std::path::PathBuf;

use tray_icon::menu::accelerator::Accelerator;
use tray_icon::menu::{IsMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{TrayIcon, TrayIconBuilder};

use stt_md::history::HistoryEntry;
use stt_md::jobs::Job;

use super::icon;

/// How many past meetings the menu offers.
pub const RECENT_NOTES: usize = 6;
const TITLE_CHARS: usize = 46;

/// What a click on the menu asks for.
#[derive(Debug, Clone)]
pub enum Action {
    Toggle,
    OpenNote(PathBuf),
    Retry(Job),
    ShowNotesFolder,
    EditConfig,
    OpenLog,
    ShowRecordings,
    ConfigureKey,
    RemoveKey,
    Quit,
}

pub struct AppMenu {
    /// First line, never clickable: what the app is doing right now.
    pub status: MenuItem,
    pub toggle: MenuItem,
    pub last_note: MenuItem,
    recent: Submenu,
    failed: Submenu,
    summary: Submenu,
    pub summarizer: MenuItem,
    pub spend: MenuItem,
    pub key: MenuItem,
    pub remove_key: MenuItem,
    settings: Submenu,
    notes_folder: MenuItem,
    edit_config: MenuItem,
    open_log: MenuItem,
    pub recordings: MenuItem,
    quit: MenuItem,
    separators: [PredefinedMenuItem; 3],
    recent_items: Vec<(MenuItem, PathBuf)>,
    failed_items: Vec<(MenuItem, Job)>,
}

impl AppMenu {
    /// `hotkey` is the global shortcut as configured ("ctrl+alt+cmd+r"), shown
    /// next to the start item when it parses.
    pub fn new(hotkey: Option<&str>) -> anyhow::Result<Self> {
        let accelerator = hotkey.and_then(|h| h.parse::<Accelerator>().ok());
        let menu = Self {
            status: MenuItem::new("Listo para grabar", false, None),
            toggle: MenuItem::new("Empezar reunión", true, accelerator),
            last_note: MenuItem::new("Abrir última nota", false, None),
            recent: Submenu::new("Notas recientes", false),
            failed: Submenu::new("Sin procesar", true),
            summary: Submenu::new("Resumen", true),
            summarizer: MenuItem::new("", false, None),
            spend: MenuItem::new("", false, None),
            key: MenuItem::new("", true, None),
            remove_key: MenuItem::new("Quitar API key de Gemini", false, None),
            settings: Submenu::new("Ajustes", true),
            notes_folder: MenuItem::new("Mostrar carpeta de notas", true, None),
            edit_config: MenuItem::new("Editar configuración…", true, None),
            open_log: MenuItem::new("Ver registro", true, None),
            recordings: MenuItem::new("Mostrar grabaciones", true, None),
            quit: MenuItem::new("Salir", true, None),
            separators: [
                PredefinedMenuItem::separator(),
                PredefinedMenuItem::separator(),
                PredefinedMenuItem::separator(),
            ],
            recent_items: Vec::new(),
            failed_items: Vec::new(),
        };
        menu.summary
            .append_items(&[&menu.summarizer, &menu.spend, &menu.key, &menu.remove_key])?;
        menu.settings.append_items(&[
            &menu.edit_config,
            &menu.notes_folder,
            &menu.recordings,
            &menu.open_log,
        ])?;
        Ok(menu)
    }

    /// The whole menu, top to bottom. "Sin procesar" only exists while there
    /// is something to retry.
    fn root(&self) -> anyhow::Result<Menu> {
        let mut items: Vec<&dyn IsMenuItem> = vec![
            &self.status,
            &self.toggle,
            &self.separators[0],
            &self.last_note,
            &self.recent,
        ];
        if !self.failed_items.is_empty() {
            items.push(&self.failed);
        }
        items.extend_from_slice(&[
            &self.separators[1],
            &self.summary,
            &self.settings,
            &self.separators[2],
            &self.quit,
        ]);
        let menu = Menu::new();
        menu.append_items(&items)?;
        Ok(menu)
    }

    pub fn build_tray(&self) -> anyhow::Result<TrayIcon> {
        Ok(TrayIconBuilder::new()
            .with_menu(Box::new(self.root()?))
            .with_icon(icon::build_idle_icon())
            .with_tooltip("stt-md")
            .with_icon_as_template(true)
            .build()?)
    }

    pub fn action_for(&self, id: &MenuId) -> Option<Action> {
        let fixed = [
            (&self.toggle, Action::Toggle),
            (&self.notes_folder, Action::ShowNotesFolder),
            (&self.edit_config, Action::EditConfig),
            (&self.open_log, Action::OpenLog),
            (&self.recordings, Action::ShowRecordings),
            (&self.key, Action::ConfigureKey),
            (&self.remove_key, Action::RemoveKey),
            (&self.quit, Action::Quit),
        ];
        if let Some((_, action)) = fixed.into_iter().find(|(item, _)| item.id() == id) {
            return Some(action);
        }
        if self.last_note.id() == id {
            return self.recent_items.first().map(|(_, p)| Action::OpenNote(p.clone()));
        }
        if let Some((_, path)) = self.recent_items.iter().find(|(item, _)| item.id() == id) {
            return Some(Action::OpenNote(path.clone()));
        }
        self.failed_items
            .iter()
            .find(|(item, _)| item.id() == id)
            .map(|(_, job)| Action::Retry(job.clone()))
    }

    /// Fills "Abrir última nota" and "Notas recientes" (newest first).
    pub fn set_recent(&mut self, entries: &[HistoryEntry]) {
        for (item, _) in self.recent_items.drain(..) {
            let _ = self.recent.remove(&item);
        }
        for entry in entries {
            let text = format!(
                "{} · {} ({} min)",
                entry.at.format("%d-%m %H:%M"),
                shorten(&entry.title, TITLE_CHARS),
                entry.duration_min
            );
            let item = MenuItem::new(text, true, None);
            let _ = self.recent.append(&item);
            self.recent_items.push((item, entry.note_path.clone()));
        }
        self.recent.set_enabled(!entries.is_empty());
        match entries.first() {
            Some(last) => {
                self.last_note
                    .set_text(format!("Abrir última nota: {}", shorten(&last.title, TITLE_CHARS)));
                self.last_note.set_enabled(true);
            }
            None => {
                self.last_note.set_text("Abrir última nota");
                self.last_note.set_enabled(false);
            }
        }
    }

    /// Fills "Sin procesar". Returns whether the section appeared or went
    /// away, in which case the tray needs the rebuilt menu from [`Self::apply`].
    pub fn set_failed(&mut self, jobs: &[Job]) -> bool {
        let had_any = !self.failed_items.is_empty();
        for (item, _) in self.failed_items.drain(..) {
            let _ = self.failed.remove(&item);
        }
        for job in jobs {
            let item = MenuItem::new(format!("Reintentar: reunión del {}", job.label()), true, None);
            let _ = self.failed.append(&item);
            self.failed_items.push((item, job.clone()));
        }
        self.failed.set_text(format!("⚠ Sin procesar ({})", jobs.len()));
        let has_any = !jobs.is_empty();
        had_any != has_any
    }

    /// Hands the tray the menu as it stands now.
    pub fn apply(&self, tray: &TrayIcon) {
        match self.root() {
            Ok(menu) => tray.set_menu(Some(Box::new(menu))),
            Err(e) => eprintln!("[stt-md] failed to rebuild the menu: {e:?}"),
        }
    }
}

fn shorten(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let cut: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// "ctrl+alt+cmd+r" → "⌃⌥⌘R", the way macOS writes shortcuts.
pub fn pretty_hotkey(hotkey: &str) -> String {
    hotkey
        .split('+')
        .map(|token| match token.trim().to_ascii_lowercase().as_str() {
            "ctrl" | "control" => "⌃".to_string(),
            "alt" | "option" => "⌥".to_string(),
            "shift" => "⇧".to_string(),
            "cmd" | "command" | "super" | "meta" | "cmdorctrl" | "commandorcontrol" => "⌘".to_string(),
            key => key.strip_prefix("key").unwrap_or(key).to_ascii_uppercase(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkeys_read_like_macos_writes_them() {
        assert_eq!(pretty_hotkey("ctrl+alt+cmd+r"), "⌃⌥⌘R");
        assert_eq!(pretty_hotkey("Cmd+Shift+KeyM"), "⌘⇧M");
        assert_eq!(pretty_hotkey("cmd+F9"), "⌘F9");
    }

    #[test]
    fn long_titles_are_cut_on_a_character_boundary() {
        assert_eq!(shorten("corto", 10), "corto");
        assert_eq!(shorten("Simulación de retornos y valorización", 12), "Simulación…");
        assert_eq!(shorten("ñandú ñandú ñandú", 7), "ñandú…");
    }
}
