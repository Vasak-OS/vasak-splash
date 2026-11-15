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

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(connected: bool, has_mode: bool) -> ConnectorCandidate {
        ConnectorCandidate {
            connected,
            has_mode,
        }
    }

    fn rutas(l: &[&str]) -> Vec<PathBuf> {
        l.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn sin_gpus_no_hay_primaria() {
        assert_eq!(primary_card(vec![]), None);
    }

    #[test]
    fn la_primaria_es_la_de_numero_mas_bajo_sin_importar_el_orden() {
        let p = primary_card(rutas(&[
            "/dev/dri/card1",
            "/dev/dri/card0",
            "/dev/dri/card2",
        ]));
        assert_eq!(p, Some(PathBuf::from("/dev/dri/card0")));
    }

    #[test]
    fn con_una_sola_gpu_esa_es_la_primaria() {
        let p = primary_card(rutas(&["/dev/dri/card3"]));
        assert_eq!(p, Some(PathBuf::from("/dev/dri/card3")));
    }

    #[test]
    fn sin_conectores_no_hay_eleccion() {
        assert_eq!(pick_connector(&[]), None);
    }

    #[test]
    fn se_elige_el_primer_conector_enchufado_y_con_modo() {
        let c = [
            cand(false, true),
            cand(true, false),
            cand(true, true),
            cand(true, true),
        ];
        assert_eq!(pick_connector(&c), Some(2));
    }

    #[test]
    fn si_ninguno_sirve_no_se_elige_ninguno() {
        let c = [cand(false, false), cand(true, false), cand(false, true)];
        assert_eq!(pick_connector(&c), None);
    }
}
