//! Configuración de la aplicación: `~/.config/correo/config.toml` más variables de entorno.
//!
//! Precedencia de los client IDs: variable de entorno > fichero > ausente.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Días de cabeceras a sincronizar por defecto.
pub const DEFAULT_SYNC_DAYS: u32 = 14;
pub const ENV_GOOGLE_CLIENT_ID: &str = "CORREO_GOOGLE_CLIENT_ID";
pub const ENV_MICROSOFT_CLIENT_ID: &str = "CORREO_MICROSOFT_CLIENT_ID";
pub const ENV_GOOGLE_CLIENT_SECRET: &str = "CORREO_GOOGLE_CLIENT_SECRET";
pub const ENV_MICROSOFT_CLIENT_SECRET: &str = "CORREO_MICROSOFT_CLIENT_SECRET";
pub const ENV_MICROSOFT_TENANT: &str = "CORREO_MICROSOFT_TENANT";

pub const ENV_HTML_RENDERER: &str = "CORREO_HTML_RENDERER";
pub const ENV_HTML_DARK_MODE: &str = "CORREO_HTML_DARK_MODE";

/// Renderizador de los mensajes con HTML. Sin la feature `webkit` siempre se usa el ligero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HtmlRenderer {
    /// WebKit para todo mensaje con HTML (los de texto plano usan siempre el visor ligero).
    #[default]
    Webkit,
    /// Visor ligero (ammonia + TextView).
    Light,
    /// WebKit solo si el HTML tiene tablas, imágenes, estilos u otros elementos de maquetación.
    Auto,
}

impl HtmlRenderer {
    /// Analiza `webkit`, `light` o `auto` (sin distinguir mayúsculas).
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "webkit" => Some(Self::Webkit),
            "light" => Some(Self::Light),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }
}

/// Un valor desconocido en el fichero no invalida el resto de la configuración.
impl<'de> Deserialize<'de> for HtmlRenderer {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        Ok(Self::parse(&text).unwrap_or_default())
    }
}

/// Qué hace la vista completa con los correos HTML cuando el tema del sistema es oscuro.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HtmlDarkMode {
    /// Los correos conscientes del modo oscuro usan sus propios colores oscuros; los demás se
    /// oscurecen automáticamente; los que ya traen fondo oscuro se dejan como están.
    #[default]
    Auto,
    /// Colores originales: siempre sobre una tarjeta blanca.
    Original,
    /// Oscurización automática siempre (salvo en correos que ya traen fondo oscuro).
    Invert,
}

impl HtmlDarkMode {
    /// Analiza `auto`, `original` o `invert` (sin distinguir mayúsculas).
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "original" => Some(Self::Original),
            "invert" => Some(Self::Invert),
            _ => None,
        }
    }
}

/// Un valor desconocido en el fichero no invalida el resto de la configuración.
impl<'de> Deserialize<'de> for HtmlDarkMode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        Ok(Self::parse(&text).unwrap_or_default())
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Config {
    /// `"webkit"` (por defecto), `"light"` o `"auto"`. Ver `HtmlRenderer`.
    #[cfg_attr(not(feature = "webkit"), allow(dead_code))]
    pub html_renderer: HtmlRenderer,
    /// `"auto"` (por defecto), `"original"` o `"invert"`: la vista completa en tema oscuro.
    #[cfg_attr(not(feature = "webkit"), allow(dead_code))]
    pub html_dark_mode: HtmlDarkMode,
    /// Días de cabeceras a sincronizar al crear una cuenta.
    pub sync_days: u32,
    /// Mínimo de mensajes recientes que se mantienen en caché por carpeta aunque sean más
    /// antiguos que la ventana de días (se valida a 20..=2000; ver `min_messages`).
    pub min_messages_per_folder: u32,
    /// Modo residente: la app sigue activa (con notificaciones) sin ventanas abiertas.
    pub run_in_background: bool,
    pub google: ProviderConfig,
    pub microsoft: ProviderConfig,
}

#[derive(Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ProviderConfig {
    pub client_id: Option<String>,
    /// Necesario en la práctica para Google (aplicación de escritorio). En apps de escritorio
    /// el secreto no es confidencial; ver docs/OAUTH.md.
    pub client_secret: Option<String>,
    /// Solo Microsoft: tenant de la organización (GUID o dominio) si la app se registró como
    /// de un solo inquilino. Por defecto `common`.
    pub tenant: Option<String>,
}

/// El secreto no aparece nunca en los logs ni en `{:?}`.
impl std::fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderConfig")
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "<redacted>"),
            )
            .field("tenant", &self.tenant)
            .finish()
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sync_days: DEFAULT_SYNC_DAYS,
            min_messages_per_folder: correo_imap::DEFAULT_MIN_MESSAGES,
            run_in_background: false,
            html_renderer: HtmlRenderer::default(),
            html_dark_mode: HtmlDarkMode::default(),
            google: ProviderConfig::default(),
            microsoft: ProviderConfig::default(),
        }
    }
}

impl Config {
    /// Analiza el contenido TOML. Los campos ausentes toman su valor por defecto.
    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// Aplica las variables de entorno sobre los valores del fichero. `var` es el
    /// lector de entorno (inyectable en tests). Un valor vacío se ignora.
    pub fn apply_env(&mut self, var: impl Fn(&str) -> Option<String>) {
        if let Some(mode) = var(ENV_HTML_RENDERER).and_then(|v| HtmlRenderer::parse(&v)) {
            self.html_renderer = mode;
        }
        if let Some(mode) = var(ENV_HTML_DARK_MODE).and_then(|v| HtmlDarkMode::parse(&v)) {
            self.html_dark_mode = mode;
        }
        if let Some(id) = var(ENV_GOOGLE_CLIENT_ID).filter(|v| !v.trim().is_empty()) {
            self.google.client_id = Some(id.trim().to_owned());
        }
        if let Some(id) = var(ENV_MICROSOFT_CLIENT_ID).filter(|v| !v.trim().is_empty()) {
            self.microsoft.client_id = Some(id.trim().to_owned());
        }
        if let Some(tenant) = var(ENV_MICROSOFT_TENANT).filter(|v| !v.trim().is_empty()) {
            self.microsoft.tenant = Some(tenant.trim().to_owned());
        }
        if let Some(secret) = var(ENV_GOOGLE_CLIENT_SECRET).filter(|v| !v.trim().is_empty()) {
            self.google.client_secret = Some(secret.trim().to_owned());
        }
        if let Some(secret) = var(ENV_MICROSOFT_CLIENT_SECRET).filter(|v| !v.trim().is_empty()) {
            self.microsoft.client_secret = Some(secret.trim().to_owned());
        }
    }

    /// Mínimo de mensajes por carpeta, ajustado al rango válido.
    pub fn min_messages(&self) -> usize {
        correo_imap::effective_min_messages(self.min_messages_per_folder)
    }

    /// Client ID de Google, si está configurado y no está vacío.
    pub fn google_client_id(&self) -> Option<&str> {
        non_empty(self.google.client_id.as_deref())
    }

    /// Client ID de Microsoft, si está configurado y no está vacío.
    pub fn microsoft_client_id(&self) -> Option<&str> {
        non_empty(self.microsoft.client_id.as_deref())
    }

    /// Client secret de Google, si está configurado y no está vacío.
    pub fn microsoft_tenant(&self) -> Option<&str> {
        non_empty(self.microsoft.tenant.as_deref())
    }

    pub fn google_client_secret(&self) -> Option<&str> {
        non_empty(self.google.client_secret.as_deref())
    }

    /// Client secret de Microsoft, si está configurado y no está vacío.
    pub fn microsoft_client_secret(&self) -> Option<&str> {
        non_empty(self.microsoft.client_secret.as_deref())
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

/// Ruta del fichero de configuración (`$XDG_CONFIG_HOME/correo/config.toml`).
pub fn config_path() -> PathBuf {
    gtk::glib::user_config_dir()
        .join("correo")
        .join("config.toml")
}

/// Carga el fichero (si no existe, valores por defecto) y aplica el entorno.
pub fn load(path: &Path) -> Result<Config, Box<dyn std::error::Error>> {
    let mut config = match std::fs::read_to_string(path) {
        Ok(text) => Config::parse(&text)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(e) => return Err(Box::new(e)),
    };
    config.apply_env(|name| std::env::var(name).ok());
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_file_is_empty() {
        let config = Config::parse("").unwrap();
        assert_eq!(config.sync_days, 14);
        assert!(!config.run_in_background);
        assert_eq!(config.google_client_id(), None);
        assert_eq!(config.microsoft_client_id(), None);
    }

    #[test]
    fn parses_client_ids_and_overrides() {
        let config = Config::parse(
            r#"
            sync_days = 30
            run_in_background = true

            [google]
            client_id = "g-123.apps.googleusercontent.com"

            [microsoft]
            client_id = "  "
            "#,
        )
        .unwrap();
        assert_eq!(config.sync_days, 30);
        assert!(config.run_in_background);
        assert_eq!(
            config.google_client_id(),
            Some("g-123.apps.googleusercontent.com")
        );
        assert_eq!(
            config.microsoft_client_id(),
            None,
            "blanco equivale a ausente"
        );
    }

    #[test]
    fn env_overrides_file_values() {
        let mut config = Config::parse("[google]\nclient_id = \"from-file\"\n").unwrap();
        config.apply_env(|name| match name {
            ENV_GOOGLE_CLIENT_ID => Some(" from-env ".into()),
            ENV_MICROSOFT_CLIENT_ID => Some(String::new()),
            _ => None,
        });
        assert_eq!(config.google_client_id(), Some("from-env"));
        assert_eq!(config.microsoft_client_id(), None);
    }

    #[test]
    fn env_sets_microsoft_when_file_has_nothing() {
        let mut config = Config::default();
        config.apply_env(|name| (name == ENV_MICROSOFT_CLIENT_ID).then(|| "ms-id".to_owned()));
        assert_eq!(config.microsoft_client_id(), Some("ms-id"));
    }

    #[test]
    fn parses_client_secrets_and_treats_blank_as_absent() {
        let config = Config::parse(
            r#"
            [google]
            client_id = "g"
            client_secret = "GOCSPX-abc"

            [microsoft]
            client_secret = "   "
            "#,
        )
        .unwrap();
        assert_eq!(config.google_client_secret(), Some("GOCSPX-abc"));
        assert_eq!(config.microsoft_client_secret(), None);
    }

    #[test]
    fn env_sets_client_secrets_and_overrides_file() {
        let mut config = Config::parse("[google]\nclient_secret = \"from-file\"\n").unwrap();
        config.apply_env(|name| match name {
            ENV_GOOGLE_CLIENT_SECRET => Some(" from-env ".into()),
            ENV_MICROSOFT_CLIENT_SECRET => Some("ms-secret".into()),
            _ => None,
        });
        assert_eq!(config.google_client_secret(), Some("from-env"));
        assert_eq!(config.microsoft_client_secret(), Some("ms-secret"));
    }

    #[test]
    fn debug_output_redacts_client_secret() {
        let config = Config::parse("[google]\nclient_secret = \"GOCSPX-supersecreto\"\n").unwrap();
        let shown = format!("{config:?}");
        assert!(!shown.contains("supersecreto"));
        assert!(shown.contains("<redacted>"));
    }

    #[test]
    fn html_renderer_defaults_to_webkit_and_parses_values() {
        assert_eq!(Config::default().html_renderer, HtmlRenderer::Webkit);
        for (text, mode) in [
            ("light", HtmlRenderer::Light),
            ("auto", HtmlRenderer::Auto),
            ("WebKit", HtmlRenderer::Webkit),
        ] {
            let config = Config::parse(&format!("html_renderer = \"{text}\"")).unwrap();
            assert_eq!(config.html_renderer, mode);
        }
        // Un valor desconocido cae al valor por defecto sin romper el fichero.
        let config = Config::parse("html_renderer = \"raro\"\nsync_days = 3").unwrap();
        assert_eq!(config.html_renderer, HtmlRenderer::Webkit);
        assert_eq!(config.sync_days, 3);
    }

    #[test]
    fn env_overrides_html_renderer_and_ignores_garbage() {
        let mut config = Config::parse("html_renderer = \"auto\"").unwrap();
        config.apply_env(|n| (n == ENV_HTML_RENDERER).then(|| " Light ".to_owned()));
        assert_eq!(config.html_renderer, HtmlRenderer::Light);
        config.apply_env(|n| (n == ENV_HTML_RENDERER).then(|| "nada".to_owned()));
        assert_eq!(config.html_renderer, HtmlRenderer::Light);
    }

    #[test]
    fn html_dark_mode_defaults_to_auto_and_parses_values() {
        assert_eq!(Config::default().html_dark_mode, HtmlDarkMode::Auto);
        for (text, mode) in [
            ("original", HtmlDarkMode::Original),
            ("Invert", HtmlDarkMode::Invert),
            ("auto", HtmlDarkMode::Auto),
        ] {
            let config = Config::parse(&format!("html_dark_mode = \"{text}\"")).unwrap();
            assert_eq!(config.html_dark_mode, mode);
        }
        let config = Config::parse("html_dark_mode = \"raro\"\nsync_days = 3").unwrap();
        assert_eq!(config.html_dark_mode, HtmlDarkMode::Auto);
        assert_eq!(config.sync_days, 3);
    }

    #[test]
    fn env_overrides_html_dark_mode_and_ignores_garbage() {
        let mut config = Config::parse("html_dark_mode = \"invert\"").unwrap();
        config.apply_env(|n| (n == ENV_HTML_DARK_MODE).then(|| " ORIGINAL ".to_owned()));
        assert_eq!(config.html_dark_mode, HtmlDarkMode::Original);
        config.apply_env(|n| (n == ENV_HTML_DARK_MODE).then(|| "nada".to_owned()));
        assert_eq!(config.html_dark_mode, HtmlDarkMode::Original);
    }

    #[test]
    fn min_messages_per_folder_is_validated() {
        assert_eq!(Config::default().min_messages(), 100);
        let parse = |v: &str| Config::parse(&format!("min_messages_per_folder = {v}")).unwrap();
        assert_eq!(parse("250").min_messages(), 250);
        assert_eq!(parse("5").min_messages(), 20);
        assert_eq!(parse("100000").min_messages(), 2000);
    }

    #[test]
    fn rejects_invalid_toml() {
        assert!(Config::parse("sync_days = [").is_err());
    }
}
