# vasak-splash

El splash de arranque de VasakOS: dibuja el logo sobre un fondo sólido en el
instante en que todavía no hay compositor, para que el arranque no muestre la
consola en texto. Propio y eficiente a propósito —nada de plymouth—: DRM directo
con *dumb buffers*, sin GBM, sin GL, sin motor de temas ni scripting.

## Qué hace, y qué no (Fase 1)

VasakOS tiene dos huecos sin nadie dibujando: **boot→greeter** (del initramfs
hasta que el greeter aparece) y **greeter→compositor** (cuando el greeter suelta
la pantalla y Wayfire todavía no la tomó).

**La Fase 1 cubre sólo el primer hueco.** El daemon arranca en el initramfs,
toma el DRM apenas el hook `kms` cargó el driver de modesetting, dibuja, y se
retira cuando el greeter le manda `quit`. El segundo hueco (Fase 2) deja el
subcomando `resume` como stub.

Alcance del hardware en v1: sólo la GPU primaria; el hotplug se ignora; el logo
va centrado por conector; y el disco cifrado queda afuera —si lo hay, el pedido
de frase de `cryptsetup` sigue siendo el texto de siempre—.

## Cómo está armado

El camino de ioctls de DRM no se puede probar sin hardware, así que toda la
lógica que **sí** se puede probar vive detrás de rasgos, con dobles en las
pruebas:

| Pieza | Archivo | Qué decide |
|---|---|---|
| Selección de GPU y conector | `src/select.rs` | la GPU primaria; el primer conector enchufado con modo |
| Geometría del blit | `src/geometry.rs` | dónde cae el logo (centrado) |
| Dibujo | `src/render.rs` | relleno del fondo y composición del logo por alfa, respetando el stride |
| Config | `src/config.rs` | parseo de `/etc/vasak-splash.conf` (color, temporizador) |
| Protocolo de control | `src/control.rs` | parseo de las órdenes del socket |
| El lazo | `src/run.rs` | tomar/soltar el DRM, dibujar, esperar el `quit` o el **temporizador de seguridad** (reloj inyectado) |
| DRM real | `src/drm_backend.rs` | la implementación con drm-rs del rasgo `Backend` (no se prueba sin hardware) |

El logo se rasteriza a RGBA crudo en `build.rs` desde `assets/logo.png` y se
embebe con `include_bytes!`: en ejecución no se decodifica nada.

### La red de seguridad

Si el `quit` no llega, el daemon **se retira solo** a los 20 s (configurable):
suelta el DRM y sale, para no dejar la pantalla negra colgada. Y si no logra
tomar el DRM de forma sostenida, se rinde y deja la consola —mejor eso que trabar
el arranque—.

## El binario

`vasak-splashd`:

- sin argumentos (o `start`): **es** el splash —toma el DRM, dibuja y espera—;
- `quit`: le dice al daemon que se retire (handoff al greeter);
- `resume`: Fase 2 (stub);
- `version`, `help`.

El control va por el socket unix `/run/vasak-splash.sock` (0660
`root:vasak-splash`).

## El `quit` lo manda el greeter (punto de integración)

En la Fase 1 el `quit` del hueco boot→greeter lo tiene que mandar el greeter de
`vasak-session-manager` **apenas dibuja su primer frame**, con
`vasak-splashd quit`. Eso **todavía no está cableado**: es un cambio en
`vasak-session-manager` (y poner al usuario `greeter` en el grupo
`vasak-splash`), que va como seguimiento. Mientras tanto, la red de seguridad
—el temporizador— garantiza que el splash no quede colgado.

## Empaquetado

Lo arma `PKGBUILDS/vasak-splash` (va directo a `main`). Entrega:

- `/usr/bin/vasak-splashd`;
- el hook de mkinitcpio (`/usr/lib/initcpio/{install,hooks}/vasak-splash`) y el
  drop-in `50-vasak-splash.conf`, que mete `vasak-splash` tras `kms` en `HOOKS`;
- `vasak-splash-start.service` (apagado: camino alternativo al del initramfs);
- el grupo `vasak-splash` (sysusers);
- `/etc/vasak-splash.conf` de ejemplo.

## Pendiente de validar en VM y hardware

El camino de DRM no se verifica desde una sesión de desarrollo. Queda por
comprobar en VM y en equipo real:

- que el logo aparezca al arrancar, sobre fondo sólido, centrado;
- el tamaño del initramfs con el hook adentro (`lsinitcpio -a`);
- el tiempo al primer frame (TTFF);
- que `vasak-splashd` **sobreviva el `switch_root`** y que el socket de `/run`
  siga alcanzable desde el sistema real;
- que **no quede negro colgado**: reteniendo el `quit` a propósito, el
  temporizador tiene que retirar el splash;
- que el handoff al greeter no parpadee (DROP_MASTER del saliente justo antes del
  SET_MASTER del entrante; vt en modo gráfico de punta a punta).
