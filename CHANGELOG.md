# Changelog

Todas las novedades de este repositorio, por paquete.

## vasak-splash 0.2.0

- El splash ahora coopera con el cambio de consola (VT): pone el vt en
  `VT_PROCESS` y, en cada cambio, suelta el master del DRM antes de ceder la
  pantalla y lo retoma al volver. Sin esto, mientras el splash estaba arriba
  ningún TTY se dibujaba —se perdía el acceso a la consola—. La consola se fija a
  su propio VT (no al flotante `/dev/tty0`), así que restaurar el modo texto y el
  `VT_SETMODE` caen sobre el VT del splash aunque el usuario haya cambiado de
  consola; y un redibujo que quedó viejo tras otro cambio de VT se descarta en vez
  de volver a tomar el master y robarle la pantalla al getty entrante.
- El logo embebido es el de VasakOS (el lince), no el de Tauri que venía por
  error en el asset.

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
