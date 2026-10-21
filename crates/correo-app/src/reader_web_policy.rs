//! Política de URLs de la vista «Ver original» (T10b). Funciones puras, sin GTK ni WebKit.
//! Se compila con la feature `webkit` y en los tests; la aplicación real la hace cumplir
//! `reader_web.rs` mediante `decide_policy` y un filtro de contenido de WebKit.

use serde_json::json;

/// Qué hacer con una navegación de nivel superior (enlace, formulario, `window.open`…).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavDecision {
    /// Cargar en el propio WebView (solo `about:` y `data:`, el HTML inicial).
    Allow,
    /// Cancelar y abrir con el lanzador de GTK (navegador o cliente de correo).
    OpenExternal,
    /// Cancelar sin más.
    Block,
}

/// Esquema de una URL en minúsculas, o cadena vacía si no tiene. Ignora espacios y
/// caracteres de control iniciales y los tabuladores/saltos de línea que WebKit también ignora.
pub fn scheme_of(uri: &str) -> String {
    let cleaned: String = uri
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    let trimmed =
        cleaned.trim_start_matches(|c: char| c.is_ascii_whitespace() || c.is_ascii_control());
    match trimmed.split_once(':') {
        Some((scheme, _))
            if !scheme.is_empty()
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) =>
        {
            scheme.to_ascii_lowercase()
        }
        _ => String::new(),
    }
}

/// Decide una navegación de nivel superior. `link_clicked` indica si la inició el usuario al
/// pulsar un enlace: solo esas navegaciones a web o correo se entregan al sistema.
pub fn navigation_decision(uri: &str, link_clicked: bool) -> NavDecision {
    match scheme_of(uri).as_str() {
        "about" | "data" => NavDecision::Allow,
        "http" | "https" | "mailto" if link_clicked => NavDecision::OpenExternal,
        _ => NavDecision::Block,
    }
}

/// Una respuesta de red solo puede pintarse si es local (`about:`, `data:`) o un recurso
/// `cid:` ya resuelto. Cualquier otra respuesta de un documento se descarta.
pub fn response_allowed(uri: &str) -> bool {
    matches!(scheme_of(uri).as_str(), "about" | "data" | "cid")
}

/// Reglas de contenido de WebKit (JSON) para los recursos secundarios (imágenes, hojas de
/// estilo, fuentes, XHR…). Bloquean todo esquema de red; con `remote_images` se añade una
/// excepción únicamente para imágenes `https`.
pub fn content_filter_json(remote_images: bool) -> String {
    let mut rules = vec![
        json!({"trigger": {"url-filter": "^https?://"}, "action": {"type": "block"}}),
        json!({"trigger": {"url-filter": "^wss?://"}, "action": {"type": "block"}}),
        json!({"trigger": {"url-filter": "^ftp://"}, "action": {"type": "block"}}),
        json!({"trigger": {"url-filter": "^file:"}, "action": {"type": "block"}}),
    ];
    if remote_images {
        rules.push(json!({
            "trigger": {"url-filter": "^https://", "resource-type": ["image"]},
            "action": {"type": "ignore-previous-rules"}
        }));
    }
    serde_json::Value::Array(rules).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheme_is_lowercased_and_tolerates_noise() {
        assert_eq!(scheme_of("HTTPS://x.org"), "https");
        assert_eq!(scheme_of("  \thttps://x.org"), "https");
        assert_eq!(scheme_of("java\tscript:alert(1)"), "javascript");
        assert_eq!(scheme_of("/relativa/ruta"), "");
        assert_eq!(scheme_of("sin esquema"), "");
        assert_eq!(scheme_of(":vacio"), "");
    }

    #[test]
    fn local_documents_load_in_place() {
        assert_eq!(
            navigation_decision("about:blank", false),
            NavDecision::Allow
        );
        assert_eq!(
            navigation_decision("data:text/html,hola", false),
            NavDecision::Allow
        );
    }

    #[test]
    fn clicked_web_and_mail_links_go_to_the_system() {
        assert_eq!(
            navigation_decision("https://ejemplo.org/x", true),
            NavDecision::OpenExternal
        );
        assert_eq!(
            navigation_decision("http://ejemplo.org", true),
            NavDecision::OpenExternal
        );
        assert_eq!(
            navigation_decision("mailto:alguien@ejemplo.org", true),
            NavDecision::OpenExternal
        );
    }

    #[test]
    fn automatic_web_navigations_are_blocked() {
        // Redirecciones, meta refresh o envíos de formulario sin clic del usuario.
        assert_eq!(
            navigation_decision("https://ejemplo.org", false),
            NavDecision::Block
        );
        assert_eq!(
            navigation_decision("http://ejemplo.org", false),
            NavDecision::Block
        );
    }

    #[test]
    fn dangerous_schemes_are_always_blocked() {
        for uri in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "ftp://ejemplo.org",
            "cid:imagen@1",
            "ws://ejemplo.org",
            "",
        ] {
            assert_eq!(navigation_decision(uri, true), NavDecision::Block, "{uri}");
        }
    }

    #[test]
    fn only_local_responses_are_rendered() {
        assert!(response_allowed("about:blank"));
        assert!(response_allowed("data:image/png;base64,AAAA"));
        assert!(response_allowed("cid:logo@ejemplo"));
        assert!(!response_allowed("https://ejemplo.org/pixel.gif"));
        assert!(!response_allowed("http://ejemplo.org"));
        assert!(!response_allowed("file:///home/usuario/secreto"));
        assert!(!response_allowed(""));
    }

    #[test]
    fn strict_filter_blocks_every_network_scheme() {
        let rules: serde_json::Value =
            serde_json::from_str(&content_filter_json(false)).expect("JSON válido");
        let rules = rules.as_array().expect("lista de reglas");
        assert_eq!(rules.len(), 4);
        assert!(rules.iter().all(|r| r["action"]["type"] == "block"));
        let filters: Vec<&str> = rules
            .iter()
            .filter_map(|r| r["trigger"]["url-filter"].as_str())
            .collect();
        assert!(filters.contains(&"^https?://"));
        assert!(filters.contains(&"^file:"));
    }

    #[test]
    fn remote_images_rule_only_unblocks_https_images() {
        let rules: serde_json::Value =
            serde_json::from_str(&content_filter_json(true)).expect("JSON válido");
        let rules = rules.as_array().expect("lista de reglas");
        assert_eq!(rules.len(), 5);
        let exception = &rules[4];
        assert_eq!(exception["action"]["type"], "ignore-previous-rules");
        assert_eq!(exception["trigger"]["url-filter"], "^https://");
        assert_eq!(exception["trigger"]["resource-type"], json!(["image"]));
    }
}
