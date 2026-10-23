//! Máquina de estados del documento electrónico.
//!
//! Toda transición se aplica en la base de datos con una condición sobre el estado actual
//! (`UPDATE ... WHERE status = ANY(predecesores)`), de modo que dos procesos concurrentes no
//! puedan pisarse: solo uno gana y el otro detecta que el documento ya cambió.

use crate::models::DocumentStatus::{self, *};

impl DocumentStatus {
    /// Estados desde los que se permite llegar a `self`.
    pub fn allowed_predecessors(self) -> &'static [DocumentStatus] {
        match self {
            // Reproceso de contingencia: vuelve a la cola de transmisión.
            Pending => &[ContingencyPending],
            SentDian => &[Pending, ContingencyPending],
            DianAccepted => &[Pending, SentDian, ContingencyPending],
            DianRejected => &[Pending, SentDian, ContingencyPending],
            ContingencyPending => &[Pending],
            ProcessingFailed => &[Pending, ContingencyPending],
            Draft | ContingencySent => &[],
        }
    }

    pub fn can_transition_to(self, next: DocumentStatus) -> bool {
        next.allowed_predecessors().contains(&self)
    }

    /// Estados finales: ningún trabajo automático los vuelve a modificar.
    pub fn is_terminal(self) -> bool {
        matches!(self, DianAccepted | DianRejected | ProcessingFailed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_states_have_no_outgoing_transitions() {
        for from in DocumentStatus::ALL.iter().filter(|s| s.is_terminal()) {
            for to in DocumentStatus::ALL {
                assert!(!from.can_transition_to(*to), "{:?} -> {:?} must be forbidden", from, to);
            }
        }
    }

    #[test]
    fn sent_document_cannot_regress_to_contingency() {
        // Hallazgo R-11: un documento ya enviado no debe volver a contingencia.
        assert!(!SentDian.can_transition_to(ContingencyPending));
        assert!(!SentDian.can_transition_to(Pending));
    }

    #[test]
    fn happy_paths_are_allowed() {
        assert!(Pending.can_transition_to(DianAccepted));
        assert!(Pending.can_transition_to(SentDian));
        assert!(SentDian.can_transition_to(DianAccepted));
        assert!(Pending.can_transition_to(ContingencyPending));
        assert!(ContingencyPending.can_transition_to(Pending));
        assert!(Pending.can_transition_to(ProcessingFailed));
    }
}
