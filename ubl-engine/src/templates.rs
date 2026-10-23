use crate::error::UblError;
use minijinja::Environment;
use serde::Serialize;
use std::sync::OnceLock;

/// Plantillas UBL compiladas una sola vez por proceso.
pub struct TemplateEngine {
    env: Environment<'static>,
}

impl Default for TemplateEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Motor compartido: las plantillas son estáticas, no hace falta recompilarlas por documento.
pub fn engine() -> &'static TemplateEngine {
    static ENGINE: OnceLock<TemplateEngine> = OnceLock::new();
    ENGINE.get_or_init(TemplateEngine::new)
}

impl TemplateEngine {
    pub fn new() -> Self {
        // Los nombres terminan en `.xml` para que MiniJinja active el autoescape: todos los
        // valores interpolados (nombres, NIT, XML embebido) se escapan como texto.
        let mut env = Environment::new();
        for (name, source) in [
            ("macros.xml", include_str!("../templates/macros.xml")),
            ("invoice.xml", include_str!("../templates/invoice_template.xml")),
            ("credit_note.xml", include_str!("../templates/credit_note_template.xml")),
            ("debit_note.xml", include_str!("../templates/debit_note_template.xml")),
            ("attached_document.xml", include_str!("../templates/attached_document_template.xml")),
            ("application_response.xml", include_str!("../templates/application_response_template.xml")),
            ("payroll.xml", include_str!("../templates/payroll_template.xml")),
        ] {
            env.add_template(name, source)
                .unwrap_or_else(|e| panic!("invalid template {}: {}", name, e));
        }
        Self { env }
    }

    /// Renderiza la plantilla `name` con el contexto dado.
    pub fn render<S: Serialize>(&self, name: &str, context: S) -> Result<String, UblError> {
        Ok(self.env.get_template(name)?.render(context)?)
    }
}
