# Logo de Correo GNOME

## Concepto

Un sobre abierto visto de frente, con una carta asomando ligeramente inclinada. El borde del bolsillo
frontal no es la «V» clásica sino una curva asimétrica (baja suave por la izquierda, sube rápida por la
derecha) que sugiere un gesto de envío: ligereza y rapidez sin recurrir a alas ni líneas de velocidad.
La línea de color de la carta hace de «asunto» y ata el papel al color de la marca. Sin letras.

El color dominante es un verde profundo y sobrio, de aire universitario.
**El verde no es un valor oficial de ninguna institución**: es un tono propio elegido para el icono.

## Paleta (base `#2b7a52`)

| Uso | Hex |
| --- | --- |
| Bolsillo frontal, degradado (arriba → abajo) | `#4c996a` → `#2b7a52` |
| Borde claro de la solapa | `#81bb91` |
| Franja base (grosor) / parte alta del interior | `#145538` |
| Fondo del interior | `#233229` |
| Sombra de la solapa sobre la carta | `#131d17` al 10 % |
| Línea de «asunto» | `#4c996a` |
| Papel, degradado | `#ffffff` → `#f6f5f4` |
| Líneas del papel | `#deddda` |
| Simbólico | `#2e3436` |

## Cambiar el color

`scripts/recolor-logo.py` (Python 3, sin dependencias) regenera el SVG a color a partir de un único
color base y deriva el resto de tonos en OKLCH:

| Tono | Regla (L, C, h del color base) |
| --- | --- |
| Degradado superior / asunto | L + 0,10 · C × 1,05 · h − 3° |
| Degradado inferior | el color base |
| Borde claro | L + 0,22 · C × 0,85 · h − 5° |
| Franja base | L − 0,12 · C × 0,8 · h + 2° |
| Interior | L 0,30 · C ≤ 0,03 (casi neutro) |
| Sombra | L 0,22 · C ≤ 0,03 |

Si un tono se sale de la gama sRGB se reduce su croma.

```sh
scripts/recolor-logo.py                         # verde por defecto, base #2b7a52
scripts/recolor-logo.py --base '#9141ac'        # otra base (p. ej. el violeta anterior)
scripts/recolor-logo.py --preview               # además regenera docs/logo-preview/*.png (inkscape)
```

El simbólico es monocromo y no depende del color. La propuesta violeta anterior se conserva como
referencia en `docs/logo-preview/alt-violet-128.png`.

## Rejilla

- Lienzo 128×128. El cuerpo del sobre ocupa x 8–120, y 40–112 (con 8 px de grosor inferior);
  la carta llega hasta y≈14. Los bordes principales caen en múltiplos de 8, de modo que quedan
  alineados a píxel a 16 y 32 px.
- Esquinas del sobre con radio 12; la carta con radio 6 y rotada −6°.
- Simbólico: 16×16, relleno único, bordes en píxeles enteros (carta con trazo de 1 px, dos líneas de
  texto de 1 px y el bolsillo con la misma curva asimétrica).

## Ficheros

- `data/icons/hicolor/scalable/apps/io.github.gmarco.Correo.svg` (generado por el script)
- `data/icons/hicolor/symbolic/apps/io.github.gmarco.Correo-symbolic.svg`
- `docs/logo-preview/io.github.gmarco.Correo-{512,128,64,32}.png`
- `scripts/recolor-logo.py`

## Regenerar las vistas previas a mano

```sh
for s in 512 128 64 32; do
  inkscape data/icons/hicolor/scalable/apps/io.github.gmarco.Correo.svg \
    -w $s -h $s -o docs/logo-preview/io.github.gmarco.Correo-$s.png
done
```

## Licencia

El icono (a color y simbólico) se publica bajo **CC0-1.0** (dominio público). Está dibujado a mano
desde cero y no incorpora elementos de terceros, ni logotipos ni colores oficiales de ninguna institución.
