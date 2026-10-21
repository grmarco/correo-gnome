# Logo de Correo GNOME

## Concepto

Un sobre abierto visto de frente, con una carta asomando ligeramente inclinada. El borde del bolsillo
frontal no es la «V» clásica sino una curva asimétrica (baja suave por la izquierda, sube rápida por la
derecha) que sugiere un gesto de envío: ligereza y rapidez sin recurrir a alas ni líneas de velocidad.
La línea violeta de la carta hace de «asunto» y ata el papel al color de la marca. Sin letras.

El violeta como color dominante distingue el icono de los clientes de correo habituales (azules).

## Paleta (paleta oficial de GNOME)

| Uso | Hex |
| --- | --- |
| Bolsillo frontal, degradado | `#c061cb` → `#9141ac` (Purple 2 → 3) |
| Brillo del borde de la solapa | `#dc8add` (Purple 1) |
| Grosor inferior / interior superior | `#613583` (Purple 5) |
| Interior del sobre (fondo) | `#3d3846` (Dark 3) |
| Sombra de la solapa sobre la carta | `#241f31` al 8 % |
| Papel, degradado | `#ffffff` → `#f6f5f4` |
| Líneas del papel | `#c061cb`, `#deddda` |
| Simbólico | `#2e3436` |

## Rejilla

- Lienzo 128×128. El cuerpo del sobre ocupa x 8–120, y 40–112 (con 8 px de grosor inferior);
  la carta llega hasta y≈14. Los bordes principales caen en múltiplos de 8, de modo que quedan
  alineados a píxel a 16 y 32 px.
- Esquinas del sobre con radio 12; la carta con radio 6 y rotada −6°.
- Simbólico: 16×16, relleno único, bordes en píxeles enteros (carta con trazo de 1 px, dos líneas de
  texto de 1 px y el bolsillo con la misma curva asimétrica).

## Ficheros

- `data/icons/hicolor/scalable/apps/io.github.gmarco.Correo.svg`
- `data/icons/hicolor/symbolic/apps/io.github.gmarco.Correo-symbolic.svg`
- `docs/logo-preview/io.github.gmarco.Correo-{512,128,64,32}.png`

## Regenerar las vistas previas

```sh
for s in 512 128 64 32; do
  inkscape data/icons/hicolor/scalable/apps/io.github.gmarco.Correo.svg \
    -w $s -h $s -o docs/logo-preview/io.github.gmarco.Correo-$s.png
done
```

(Con `rsvg-convert -w $s -h $s fichero.svg -o salida.png` el resultado es equivalente.)

## Licencia

El icono (versión a color y simbólica) se publica bajo **CC0-1.0** (dominio público). Está dibujado
a mano desde cero y no incorpora elementos de terceros.
