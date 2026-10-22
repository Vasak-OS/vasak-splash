//! Selección de GPU y de conector. La política vive acá, en funciones puras,
//! para poder probarla con listas armadas a mano; el backend real sólo enumera
//! el hardware y le pasa el resultado.
//!
//! Alcance v1 (del diseño): una sola GPU, la primaria; nada de hotplug.

use std::path::PathBuf;

/// Elige la GPU primaria entre los nodos de `/dev/dri`. Hoy es "la de número más
/// bajo" (`card0` antes que `card1`): en la enorme mayoría de los equipos la
/// primaria es `card0`, y el alcance v1 es justamente esa. Consume la lista y la
/// ordena para no depender del orden en que el sistema de archivos los devuelva.
pub fn primary_card(mut cards: Vec<PathBuf>) -> Option<PathBuf> {
    cards.sort();
    cards.into_iter().next()
}

/// Lo que hace falta saber de un conector para decidir si dibujar en él: si está
/// enchufado y si ofrece algún modo. El backend real lo arma desde
/// `connector::Info`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectorCandidate {
    pub connected: bool,
    pub has_mode: bool,
}

/// Índice del primer conector enchufado y con modo. `None` si ninguno sirve
/// —una pantalla apagada o sin modo no es donde dibujar el splash—.
pub fn pick_connector(candidates: &[ConnectorCandidate]) -> Option<usize> {
    candidates.iter().position(|c| c.connected && c.has_mode)
}
