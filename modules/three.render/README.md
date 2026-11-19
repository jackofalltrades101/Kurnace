# three.render — declarative three.js scenes

Renders a three.js scene in headless Chromium to a PNG frame sequence
(spec §12). No WebGL native addon is needed; it uses the same Chromium family
as the final renderer.

## Scene descriptor

```json
{
  "background": "#101820",
  "camera": { "position": [3, 2, 4], "lookAt": [0, 0, 0], "fov": 45 },
  "objects": [
    {
      "type": "box",
      "size": [1, 1, 1],
      "position": [0, 0, 0],
      "rotation": [0, 0, 0],
      "rotationSpeed": [0.2, 0.6, 0.1],
      "color": "#4f8cff"
    },
    { "type": "sphere", "radius": 0.5, "position": [-1.5, 0.2, 0], "color": "#ffb84f" }
  ],
  "lights": [
    { "type": "directional", "position": [3, 5, 2], "intensity": 2.5 }
  ]
}
```

- Types: `box`, `sphere`, `plane`, `torus`, `cylinder`, `cone`.
- Animation is a pure function of the frame: `rotation = base + rotationSpeed × frame`.
- Light types: `directional`, `ambient`, `point`, `hemisphere`.

```sh
kurn run three.render --scene '{"objects":[{"type":"box","rotationSpeed":[0.2,0.6,0.1]}]}' \
  --frames 120 --fps 30 --width 1920 --height 1080
# or: --file scenes/shot.json   (relative to the project root)
```

Output is a `frames` directory asset (`kind = seq`) plus a deterministic
`kurn.json` with `fps_num/fps_den`, `frames`, `width`, `height`,
`engine: three`, `language: javascript`. `render.revideo` plays it on the
timeline.
