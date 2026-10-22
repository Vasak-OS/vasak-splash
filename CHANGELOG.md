# Changelog

Todas las novedades de este repositorio, por paquete.

## vasak-splash 0.1.0

Primera versión. El splash de arranque propio de VasakOS, Fase 1: el hueco
boot→greeter.

- El daemon `vasak-splashd` toma el DRM con *dumb buffers* (sin GBM ni GL) y
  dibuja el logo de VasakOS sobre un fondo sólido. El logo va embebido como RGBA
  crudo (`build.rs` + `include_bytes!`), así que en ejecución no se decodifica
  nada.
- Control por socket unix (`/run/vasak-splash.sock`): el subcomando `quit`
  retira el splash (handoff al greeter); `resume` queda como stub de la Fase 2.
- Red de seguridad: un temporizador duro (20 s, configurable) retira el splash
  solo si el `quit` no llega, y el daemon se rinde si no logra tomar el DRM, para
  no trabar el arranque.
- Toda la lógica que no es ioctls de DRM —selección de GPU y conector, geometría
  del blit, dibujo, parseo de config y del protocolo, y el temporizador con reloj
  inyectado— vive detrás de rasgos y se prueba con dobles.
- Empaquetado: hook de mkinitcpio (tras `kms`), drop-in de `HOOKS`, unidad de
  systemd (apagada), grupo `vasak-splash` y config de ejemplo.
