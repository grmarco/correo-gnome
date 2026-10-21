//! Estado de interfaz que sobrevive entre sesiones (hoy: cuentas plegadas en la barra
//! lateral). Se guarda en `~/.local/share/correo/ui-state.json`; cualquier error de
//! lectura o escritura solo se informa por `eprintln` y nunca bloquea la interfaz.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use correo_core::AccountId;
use serde::{Deserialize, Serialize};

const FILE_NAME: &str = "ui-state.json";

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiState {
    /// Cuentas cuya lista de carpetas está plegada.
    #[serde(default)]
    collapsed_accounts: BTreeSet<AccountId>,
    /// Plegado de carpetas con hijos distinto del predeterminado, por cuenta y ruta
    /// normalizada (`true` = plegada). Ausente = el valor por defecto del nodo.
    #[serde(default)]
    folder_folds: BTreeMap<AccountId, BTreeMap<String, bool>>,
}

impl UiState {
    pub fn is_collapsed(&self, id: AccountId) -> bool {
        self.collapsed_accounts.contains(&id)
    }

    pub fn set_collapsed(&mut self, id: AccountId, collapsed: bool) {
        if collapsed {
            self.collapsed_accounts.insert(id);
        } else {
            self.collapsed_accounts.remove(&id);
        }
    }

    /// Alterna el plegado de `id` y devuelve el nuevo estado (`true` = plegada).
    pub fn toggle(&mut self, id: AccountId) -> bool {
        let collapsed = !self.is_collapsed(id);
        self.set_collapsed(id, collapsed);
        collapsed
    }

    /// Olvida las cuentas que ya no existen.
    pub fn retain_accounts(&mut self, existing: &[AccountId]) {
        self.collapsed_accounts.retain(|id| existing.contains(id));
        self.folder_folds.retain(|id, _| existing.contains(id));
    }

    /// `true` si la carpeta `key` de la cuenta está plegada; `default` si el usuario no
    /// ha elegido.
    pub fn folder_collapsed(&self, account: AccountId, key: &str, default: bool) -> bool {
        self.folder_folds
            .get(&account)
            .and_then(|m| m.get(key))
            .copied()
            .unwrap_or(default)
    }

    /// Recuerda el plegado de una carpeta (solo se guarda si difiere del predeterminado).
    pub fn set_folder_collapsed(
        &mut self,
        account: AccountId,
        key: &str,
        collapsed: bool,
        default: bool,
    ) {
        if collapsed == default {
            if let Some(m) = self.folder_folds.get_mut(&account) {
                m.remove(key);
                if m.is_empty() {
                    self.folder_folds.remove(&account);
                }
            }
        } else {
            self.folder_folds
                .entry(account)
                .or_default()
                .insert(key.to_owned(), collapsed);
        }
    }

    /// Olvida las carpetas de `account` que ya no existen (`keys` son las claves vigentes).
    pub fn retain_folders(&mut self, account: AccountId, keys: &BTreeSet<String>) {
        if let Some(m) = self.folder_folds.get_mut(&account) {
            m.retain(|k, _| keys.contains(k));
            if m.is_empty() {
                self.folder_folds.remove(&account);
            }
        }
    }

    /// Lee el estado de `dir`. Si falta o está dañado, devuelve el estado por defecto.
    pub fn load_from(dir: &Path) -> Self {
        let path = dir.join(FILE_NAME);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                eprintln!("ui-state: {} ilegible: {e}", path.display());
                Self::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                eprintln!("ui-state: no se pudo leer {}: {e}", path.display());
                Self::default()
            }
        }
    }

    /// Escribe el estado en `dir` de forma atómica (fichero temporal + `rename`).
    pub fn save_to(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("{FILE_NAME}.tmp"));
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        let result = (|| {
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&tmp, dir.join(FILE_NAME))
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }

    pub fn load() -> Self {
        Self::load_from(&data_dir())
    }

    /// Guarda en el directorio de datos desde un hilo aparte (la interfaz no espera al
    /// disco); los errores solo se imprimen. Si dos guardados se cruzan, gana el último.
    pub fn save(&self) {
        static LATEST: AtomicU64 = AtomicU64::new(0);
        static WRITING: Mutex<()> = Mutex::new(());
        let seq = LATEST.fetch_add(1, Ordering::SeqCst) + 1;
        let snapshot = self.clone();
        let dir = data_dir();
        std::thread::spawn(move || {
            let _guard = WRITING.lock().unwrap_or_else(|e| e.into_inner());
            if LATEST.load(Ordering::SeqCst) != seq {
                return; // ya hay un estado más reciente esperando
            }
            if let Err(e) = snapshot.save_to(&dir) {
                eprintln!("ui-state: no se pudo guardar: {e}");
            }
        });
    }
}

fn data_dir() -> PathBuf {
    gtk::glib::user_data_dir().join("correo")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("correo-ui-state-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn missing_file_gives_default() {
        let dir = temp_dir("missing");
        assert_eq!(UiState::load_from(&dir), UiState::default());
    }

    #[test]
    fn toggle_and_roundtrip() {
        let dir = temp_dir("roundtrip");
        let mut s = UiState::default();
        assert!(s.toggle(3));
        assert!(s.toggle(7));
        assert!(!s.toggle(3));
        s.save_to(&dir).unwrap();
        let loaded = UiState::load_from(&dir);
        assert_eq!(loaded, s);
        assert!(loaded.is_collapsed(7));
        assert!(!loaded.is_collapsed(3));
        assert!(!dir.join("ui-state.json.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_gives_default() {
        let dir = temp_dir("corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ui-state.json"), "{no es json").unwrap();
        assert_eq!(UiState::load_from(&dir), UiState::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn retain_drops_removed_accounts() {
        let mut s = UiState::default();
        s.set_collapsed(1, true);
        s.set_collapsed(2, true);
        s.retain_accounts(&[2, 5]);
        assert!(!s.is_collapsed(1));
        assert!(s.is_collapsed(2));
    }

    #[test]
    fn old_format_without_folders_still_loads() {
        let dir = temp_dir("old-format");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ui-state.json"), r#"{"collapsed_accounts":[3]}"#).unwrap();
        let s = UiState::load_from(&dir);
        assert!(s.is_collapsed(3));
        assert!(!s.folder_collapsed(3, "Archives", false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn folder_folds_store_only_overrides_and_roundtrip() {
        let dir = temp_dir("folders");
        let mut s = UiState::default();
        assert!(!s.folder_collapsed(1, "Archives", false));
        assert!(s.folder_collapsed(1, "[Gmail]", true));
        s.set_folder_collapsed(1, "Archives", true, false);
        s.set_folder_collapsed(1, "[Gmail]", false, true);
        s.set_folder_collapsed(2, "Otra", false, false); // igual al defecto: no se guarda
        s.save_to(&dir).unwrap();
        let loaded = UiState::load_from(&dir);
        assert_eq!(loaded, s);
        assert!(loaded.folder_collapsed(1, "Archives", false));
        assert!(!loaded.folder_collapsed(1, "[Gmail]", true));
        assert!(!loaded.folder_collapsed(2, "Otra", false));
        let mut s = loaded;
        s.set_folder_collapsed(1, "Archives", false, false);
        assert!(!s.folder_collapsed(1, "Archives", false));
        s.retain_accounts(&[2]);
        assert!(s.folder_collapsed(1, "[Gmail]", true));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
